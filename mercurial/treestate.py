# Copyright Facebook, Inc. 2018
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.

"""tree-based dirstate combined with other (ex. fsmonitor) states"""

from __future__ import absolute_import

import os
import uuid

from . import error, node, util
from .i18n import _
from .rust import treestate


# header after the first 40 bytes of dirstate.
HEADER = b"\ntreestate\n\0"


class _overlaydict(dict):
    def __init__(self, lookup, *args, **kwargs):
        super(_overlaydict, self).__init__(*args, **kwargs)
        self.lookup = lookup

    def get(self, key, default=None):
        s = super(_overlaydict, self)
        if s.__contains__(key):
            return s.__getitem__(key)
        r = self.lookup(key)
        if r is not None:
            return r
        return default

    def __getitem__(self, key):
        s = super(_overlaydict, self)
        if s.__contains__(key):
            return s[key]
        r = self.lookup(key)
        if r is not None:
            return r
        raise KeyError(key)


def _packmetadata(dictobj):
    result = []
    for k, v in dictobj.iteritems():
        entry = "%s=%s" % (k, v)
        if "=" in k or "\0" in entry:
            raise error.ProgrammingError("illegal metadata entry: %r" % entry)
        result.append(entry)
    return "\0".join(result)


def _unpackmetadata(data):
    return dict(entry.split("=", 1) for entry in data.split("\0") if "=" in entry)


class treestatemap(object):
    """a drop-in replacement for dirstate._map, with more abilities like also
    track fsmonitor state.

    The treestate files are stored at ".hg/treestate/<uuid>". It uses a
    Rust-backed append-only map which tracks detailed information in one tree,
    and maintains aggregated states at each tree, so copymap, nonnormalset,
    otherparentset do not need to be tracked separately, and can be calculated
    in O(log N) time. It also stores a "metadata" string, which usually are p1,
    p2, and watchman clock.

    The first 40 bytes of ".hg/dirstate" remains compatible with earlier
    Mercurial, and the remaining bytes of ".hg/dirstate" contains the "<uuid>"
    and an offset.
    """

    def __init__(self, ui, vfs, root, importdirstate=None):
        self._ui = ui
        self._vfs = vfs
        self._root = root
        if importdirstate:
            # Import from an old dirstate
            self.clear()
            self._parents = importdirstate.parents()
            self._tree.importmap(importdirstate._map)
            # Import copymap
            copymap = importdirstate.copies()
            for dest, src in copymap.iteritems():
                self.copy(src, dest)
        else:
            # The original dirstate lazily reads content for performance.
            # But our dirstate map is lazy anyway. So "_read" during init
            # should be fine.
            self._read()

    @property
    def copymap(self):
        result = {}
        for path in self._tree.walk(treestate.COPIED, 0):
            copied = self._tree.get(path, None)[-1]
            if not copied:
                raise error.Abort(
                    _(
                        "working directory state appears "
                        "damaged (wrong copied information)!"
                    )
                )
            result[path] = copied
        return result

    def clear(self):
        self._threshold = 0
        self._rootid = 0
        self._clock = ""
        self._parents = (node.nullid, node.nullid)

        # use a new file
        self._filename = "%s" % uuid.uuid4()
        path = self._vfs.join("treestate", self._filename)
        self._tree = treestate.treestate(path, self._rootid)

    def iteritems(self):
        return ((k, self[k]) for k in self.keys())

    def __iter__(self):
        return iter(self.keys())

    def __len__(self):
        return len(self._tree)

    def get(self, key, default=None):
        entry = self._tree.get(key, None)
        if entry is None or len(entry) != 5:
            return default
        flags, mode, size, mtime, _copied = entry
        # convert flags to Mercurial dirstate state
        state = treestate.tohgstate(flags)
        return (state, mode, size, mtime)

    def __contains__(self, key):
        # note: this returns False for files with "?" state.
        return key in self._tree

    def __getitem__(self, key):
        result = self.get(key)
        if result is None:
            raise KeyError(key)
        return result

    def keys(self):
        return self._tree.walk(0, 0)

    def preload(self):
        pass

    def addfile(self, f, oldstate, state, mode, size, mtime):
        if state == "n":
            if size == -2:
                state = treestate.EXIST_P2 | treestate.EXIST_NEXT
            else:
                state = treestate.EXIST_P1 | treestate.EXIST_NEXT
        elif state == "m":
            state = treestate.EXIST_P1 | treestate.EXIST_P2 | treestate.EXIST_NEXT
        elif state == "a":
            state = treestate.EXIST_NEXT
        else:
            raise error.ProgrammingError("unknown addfile state: %s" % state)
        # TODO: figure out whether "copied" needs to be preserved here.
        self._tree.insert(f, state, mode, size, mtime, None)

    def removefile(self, f, oldstate, size):
        existing = self._tree.get(f, None)
        if existing:
            # preserve "copied" information
            state, mode, size, mtime, copied = existing
            state ^= state & treestate.EXIST_NEXT
        else:
            state = 0
            copied = None
            mode = 0o666
            mtime = -1
            size = 0
        self._tree.insert(f, state, mode, size, mtime, copied)

    def dropfile(self, f, oldstate):
        return self._tree.remove(f)

    def clearambiguoustimes(self, _files, now):
        # TODO(quark): could _files be different from those with mtime = -1
        # ones?
        self._tree.invalidatemtime(now)

    def nonnormalentries(self):
        return (self.nonnormalset, self.otherparentset)

    def getfiltered(self, path, filterfunc):
        return self._tree.getfiltered(path, filterfunc, id(filterfunc))

    @property
    def filefoldmap(self):
        filterfunc = util.normcase

        def lookup(path):
            tree = self._tree
            f = tree.getfiltered(path, filterfunc, id(filterfunc))
            if f is not None and not (tree.get(f, None)[0] & treestate.EXIST_NEXT):
                f = None
            return f

        return _overlaydict(lookup)

    def hastrackeddir(self, d):
        if not d.endswith("/"):
            d += "/"
        state = self._tree.get(d, None)  # [union, intersection]
        return bool(state and (state[0] & treestate.EXIST_NEXT))

    def hasdir(self, d):
        if not d.endswith("/"):
            d += "/"
        return self._tree.hasdir(d)

    def parents(self):
        return self._parents

    def setparents(self, p1, p2):
        self._parents = (p1, p2)

    def _read(self):
        """Read every metadata automatically"""
        dirstate = self._vfs.tryread("dirstate")
        f = util.stringio(dirstate)
        p1 = f.read(20) or node.nullid
        p2 = f.read(20) or node.nullid
        header = f.read(len(HEADER))
        if header and header != HEADER:
            raise error.Abort(_("working directory state appears damaged!"))
        # simple key-value serialization
        metadata = _unpackmetadata(f.read())
        if metadata:
            try:
                # main append-only tree state filename and root offset
                filename = metadata["filename"]
                rootid = int(metadata["rootid"])
                # whether to write a new file or not during "write"
                threshold = int(metadata.get("threshold", 0))
            except (KeyError, ValueError):
                raise error.Abort(_("working directory state appears damaged!"))
        else:
            filename = "%s" % uuid.uuid4()
            rootid = 0
            threshold = 0

        self._parents = (p1, p2)
        self._threshold = threshold
        self._rootid = rootid
        self._filename = filename

        path = self._vfs.join("treestate", filename)
        tree = treestate.treestate(path, rootid)

        # Double check p1 p2 against metadata stored in the tree. This is
        # redundant but many things depend on "dirstate" file format.
        # The metadata here contains (watchman) "clock" which does not exist
        # in "dirstate".
        metadata = _unpackmetadata(tree.getmetadata())
        if metadata:
            if metadata.get("p1", node.nullhex) != node.hex(p1) or metadata.get(
                "p2", node.nullhex
            ) != node.hex(p2):
                raise error.Abort(
                    _("working directory state appears damaged (metadata mismatch)!")
                )
            clock = metadata.get("clock")
        else:
            clock = ""
        self._clock = clock
        self._tree = tree

    def write(self, st, now):
        # write .hg/treestate/<uuid>
        metadata = {}
        if self._clock:
            metadata.update(
                {
                    "clock": self._clock,
                    # for debugging purpose
                    "pid": os.getpid(),
                    "now": now,
                }
            )
        if self._parents[0] != node.nullid:
            metadata["p1"] = node.hex(self._parents[0])
        if self._parents[1] != node.nullid:
            metadata["p2"] = node.hex(self._parents[1])
        self._tree.setmetadata(_packmetadata(metadata))
        self._tree.invalidatemtime(now)

        # TODO(quark): add auto repacking - could be using another filename
        self._vfs.makedirs("treestate")
        rootid = self._tree.flush()

        # write .hg/dirstate
        st.write(self._parents[0])
        st.write(self._parents[1])
        st.write(HEADER)
        st.write(
            _packmetadata(
                {
                    "filename": self._filename,
                    "rootid": rootid,
                    "threshold": self._threshold,
                }
            )
        )
        st.close()
        self._rootid = rootid

    @property
    def nonnormalset(self):
        # not normal: hg dirstate state != 'n', or mtime == -1 (NEED_CHECK)
        tree = self._tree
        needcheck = tree.walk(treestate.NEED_CHECK, 0)
        merged = tree.walk(treestate.EXIST_P1 | treestate.EXIST_P2, 0)
        added = tree.walk(treestate.EXIST_NEXT, treestate.EXIST_P1 | treestate.EXIST_P2)
        removed = tree.walk(0, treestate.EXIST_NEXT)
        return set(needcheck + merged + added + removed)

    @property
    def otherparentset(self):
        # Only exist in P2
        return set(self._tree.walk(treestate.EXIST_P2, treestate.EXIST_P1))

    @property
    def identity(self):
        return "%s-%s" % (self._filename, self._rootid)

    @property
    def dirfoldmap(self):
        filterfunc = util.normcase

        def lookup(path):
            tree = self._tree
            f = tree.getfiltered(path + "/", filterfunc, id(filterfunc))
            if f is not None and not self.hastrackeddir(path):
                f = None
            return f.rstrip("/") if f is not None else f

        return _overlaydict(lookup)

    def copy(self, source, dest):
        if source == dest:
            return
        existing = self._tree.get(dest, None)
        if existing:
            state, mode, size, mtime, copied = existing
            if bool(copied) != bool(source):
                self._tree.insert(dest, state, mode, size, mtime, source)
        else:
            raise error.ProgrammingError("copy dest %r does not exist" % dest)
