/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![feature(try_blocks)]

pub mod git_reader;
mod gitimport_objects;
mod gitlfs;

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::str;
use std::sync::RwLock;

use anyhow::bail;
use anyhow::format_err;
use anyhow::Context;
use anyhow::Error;
use anyhow::Result;
use borrowed::borrowed;
use bytes::Bytes;
use cloned::cloned;
use context::CoreContext;
use futures::stream;
use futures::try_join;
use futures::Stream;
use futures::StreamExt;
use futures::TryFutureExt;
use futures::TryStreamExt;
use git_symbolic_refs::GitSymbolicRefsEntry;
use gix_hash::ObjectId;
use gix_object::Object;
use linked_hash_map::LinkedHashMap;
use manifest::BonsaiDiffFileChange;
use mononoke_types::ChangesetId;
use mononoke_types::FileType;
use mononoke_types::NonRootMPath;
use slog::debug;
use slog::info;
use sorted_vector_map::SortedVectorMap;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::process::Command;
use tokio::task;

use crate::git_reader::GitReader;
pub use crate::git_reader::GitRepoReader;
pub use crate::gitimport_objects::oid_to_sha1;
pub use crate::gitimport_objects::BackfillDerivation;
pub use crate::gitimport_objects::CommitMetadata;
pub use crate::gitimport_objects::ExtractedCommit;
pub use crate::gitimport_objects::GitLeaf;
pub use crate::gitimport_objects::GitManifest;
pub use crate::gitimport_objects::GitTree;
pub use crate::gitimport_objects::GitUploader;
pub use crate::gitimport_objects::GitimportPreferences;
pub use crate::gitimport_objects::GitimportTarget;
pub use crate::gitimport_objects::TagMetadata;
pub use crate::gitlfs::GitImportLfs;
pub use crate::gitlfs::LfsMetaData;

pub const HGGIT_MARKER_EXTRA: &str = "hg-git-rename-source";
pub const HGGIT_MARKER_VALUE: &[u8] = b"git";
pub const HGGIT_COMMIT_ID_EXTRA: &str = "convert_revision";
pub const BRANCH_REF: &str = "branch";
pub const TAG_REF: &str = "tag";
pub const BRANCH_REF_PREFIX: &str = "refs/heads/";
pub const TAG_REF_PREFIX: &str = "refs/tags/";

// TODO: Try to produce copy-info?
async fn find_file_changes<S, U, R>(
    ctx: &CoreContext,
    lfs: &GitImportLfs,
    reader: &R,
    uploader: U,
    changes: S,
) -> Result<SortedVectorMap<NonRootMPath, U::Change>>
where
    S: Stream<Item = Result<BonsaiDiffFileChange<GitLeaf>>>,
    U: GitUploader,
    R: GitReader,
{
    changes
        .map_ok(|change| async {
            task::spawn({
                cloned!(ctx, reader, uploader, lfs);
                async move {
                    match change {
                        BonsaiDiffFileChange::Changed(path, ty, GitLeaf(oid))
                        | BonsaiDiffFileChange::ChangedReusedId(path, ty, GitLeaf(oid)) => {
                            if ty == FileType::GitSubmodule {
                                // The OID for a submodule is a commit in another repository, so there is no data to
                                // store.
                                uploader
                                    .upload_file(&ctx, &lfs, &path, ty, oid, Bytes::new())
                                    .await
                                    .map(|change| (path, change))
                            } else {
                                let object =
                                    reader.get_object(&oid).await.context("reader.get_object")?;
                                let blob = object
                                    .parsed
                                    .try_into_blob()
                                    .map_err(|_| format_err!("{} is not a blob", oid))?;

                                let upload_packfile =
                                    uploader.upload_packfile_base_item(&ctx, oid, object.raw);
                                let upload_git_blob = uploader.upload_file(
                                    &ctx,
                                    &lfs,
                                    &path,
                                    ty,
                                    oid,
                                    Bytes::from(blob.data),
                                );
                                let (_, change) = try_join!(upload_packfile, upload_git_blob)?;
                                anyhow::Ok((path, change))
                            }
                        }
                        BonsaiDiffFileChange::Deleted(path) => Ok((path, U::deleted())),
                    }
                }
            })
            .await?
        })
        .try_buffer_unordered(100)
        .try_collect()
        .await
}

pub struct GitimportAccumulator {
    inner: LinkedHashMap<ObjectId, ChangesetId>,
}

impl GitimportAccumulator {
    pub fn new() -> Self {
        Self {
            inner: LinkedHashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn insert(&mut self, oid: ObjectId, cs_id: ChangesetId) {
        self.inner.insert(oid, cs_id);
    }

    pub fn get(&self, oid: &gix_hash::oid) -> Option<ChangesetId> {
        self.inner.get(oid).copied()
    }
}

pub fn stored_tag_name(tag_name: String) -> String {
    tag_name
        .strip_prefix("refs/")
        .map(|s| s.to_string())
        .unwrap_or(tag_name)
}

pub async fn create_changeset_for_annotated_tag<Uploader: GitUploader>(
    ctx: &CoreContext,
    uploader: &Uploader,
    path: &Path,
    prefs: &GitimportPreferences,
    tag_id: &ObjectId,
    maybe_tag_name: Option<String>,
    original_changeset_id: &ChangesetId,
) -> Result<ChangesetId> {
    let reader = GitRepoReader::new(&prefs.git_command_path, path).await?;
    // Get the parsed Git Tag
    let tag_metadata = TagMetadata::new(ctx, *tag_id, maybe_tag_name, &reader)
        .await
        .with_context(|| format_err!("Failed to create TagMetadata from git tag {}", tag_id))?;
    // Create the corresponding changeset for the Git Tag at Mononoke end
    let changeset_id = uploader
        .generate_changeset_for_annotated_tag(ctx, *original_changeset_id, tag_metadata)
        .await
        .with_context(|| format_err!("Failed to generate changeset for git tag {}", tag_id))?;
    Ok(changeset_id)
}

pub async fn upload_git_tag<Uploader: GitUploader, Reader: GitReader>(
    ctx: &CoreContext,
    uploader: &Uploader,
    reader: &Reader,
    tag_id: &ObjectId,
) -> Result<()> {
    let tag_bytes = reader
        .read_raw_object(tag_id)
        .await
        .with_context(|| format_err!("Failed to fetch git tag {}", tag_id))?;
    let raw_tag_bytes = tag_bytes.clone();
    // Upload Packfile Item for the Git Tag
    let upload_packfile = async {
        uploader
            .upload_packfile_base_item(ctx, *tag_id, tag_bytes)
            .await
            .with_context(|| format_err!("Failed to upload packfile item for git tag {}", tag_id))
    };
    // Upload Git Tag
    let upload_git_tag = async {
        uploader
            .upload_object(ctx, *tag_id, raw_tag_bytes)
            .await
            .with_context(|| format_err!("Failed to upload raw git tag {}", tag_id))
    };
    try_join!(upload_packfile, upload_git_tag)?;
    Ok(())
}

fn repo_name(prefs: &GitimportPreferences, path: &Path) -> String {
    let repo_name = if let Some(name) = &prefs.gitrepo_name {
        String::from(name)
    } else {
        let name_path = if path.ends_with(".git") {
            path.parent().unwrap_or(path)
        } else {
            path
        };
        String::from(name_path.to_string_lossy())
    };
    repo_name
}

pub async fn gitimport_acc<Uploader: GitUploader>(
    ctx: &CoreContext,
    path: &Path,
    uploader: &Uploader,
    target: &GitimportTarget,
    prefs: &GitimportPreferences,
) -> Result<GitimportAccumulator> {
    let repo_name = repo_name(prefs, path);
    let reader = GitRepoReader::new(&prefs.git_command_path, path)
        .await
        .context("GitRepoReader::new")?;
    let roots = target.get_roots();
    let all_commits = target
        .list_commits(&prefs.git_command_path, path)
        .await
        .context("target.list_commits")?;
    let nb_commits_to_import = all_commits.len();
    if 0 == nb_commits_to_import {
        info!(ctx.logger(), "Nothing to import for repo {}.", repo_name);
        return Ok(GitimportAccumulator::new());
    }
    import_commit_contents(ctx, repo_name, all_commits, roots, uploader, reader, prefs).await
}

pub async fn import_commit_contents<Uploader: GitUploader, Reader: GitReader>(
    ctx: &CoreContext,
    repo_name: String,
    all_commits: Vec<Result<ObjectId>>,
    roots: &HashMap<ObjectId, ChangesetId>,
    uploader: &Uploader,
    reader: Reader,
    prefs: &GitimportPreferences,
) -> Result<GitimportAccumulator> {
    let nb_commits_to_import = all_commits.len();
    let dry_run = prefs.dry_run;
    let acc = RwLock::new(GitimportAccumulator::new());
    let backfill_derivation = prefs.backfill_derivation.clone();

    // How many commits to query from bonsai git mapping per SQL query.
    const SQL_CONCURRENCY: usize = 10_000;
    let mappings: Vec<(ObjectId, ChangesetId)> = stream::iter(&all_commits)
        // Ignore any error. This is an optional optimization
        .filter_map(|res| async { res.as_ref().ok() })
        .chunks(SQL_CONCURRENCY)
        .map(|v| v.into_iter().cloned().collect::<Vec<_>>())
        .map(|oids| {
            borrowed!(uploader);
            async move {
                uploader
                    .preload_uploaded_commits(ctx, &oids)
                    .await
                    .context("preload_uploaded_commits")
            }
        })
        .buffered(prefs.concurrency)
        .try_collect::<Vec<_>>()
        .await?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    acc.write().expect("lock poisoned").inner.extend(mappings);
    let n_existing_commits = acc.read().expect("lock poisoned").len();
    if n_existing_commits > 0 {
        info!(
            ctx.logger(),
            "GitRepo:{} {} of {} commit(s) already exist",
            repo_name,
            n_existing_commits,
            nb_commits_to_import,
        );
    }
    // Kick off a stream that consumes the walk and prepared commits. Then, produce the Bonsais.
    stream::iter(all_commits)
        .try_filter_map({
            borrowed!(acc);
            move |oid| async move {
                if let Some(_bcs_id) = acc.read().expect("lock poisoned").get(&oid) {
                    Ok(None)
                } else {
                    Ok(Some(oid))
                }
            }
        })
        .map_ok(|oid| {
            cloned!(ctx, reader, uploader, prefs.lfs, prefs.submodules);
            async move {
                task::spawn({
                    async move {
                    let extracted_commit = ExtractedCommit::new(&ctx, oid, &reader)
                        .await
                        .with_context(|| format!("While extracting {}", oid))?;

                    let diff = extracted_commit.diff(&ctx, &reader, submodules);
                    let file_changes = find_file_changes(&ctx, &lfs, &reader, uploader, diff).await.context("find_file_changes")?;
                    Result::<_, Error>::Ok((extracted_commit, file_changes))
                    }
                })
                .await?
            }
        })
        .try_buffered(prefs.concurrency)
        .and_then(|(extracted_commit, file_changes)| {
            borrowed!(acc, repo_name: &str);
            cloned!(uploader, reader, ctx);
            async move {
                let oid = extracted_commit.metadata.oid;
                let bonsai_parents = extracted_commit
                    .metadata
                    .parents
                    .iter()
                    .map(|p| {
                        roots
                            .get(p)
                            .copied()
                            .or_else(|| acc.read().expect("lock poisoned").get(p))
                            .ok_or_else(|| {
                                format_err!(
                                    "Couldn't find parent: {} in local list of imported commits",
                                    p
                                )
                            })
                    })
                    .collect::<Result<Vec<_>>>()
                    .with_context(|| format_err!("While looking for parents of {}", oid))?;

                // Before generating the corresponding changeset at Mononoke end, upload the raw git commit
                // and the git tree pointed to by the git commit.
                extracted_commit
                    .changed_trees(&ctx, &reader)
                    .map_ok(|entry| {
                        cloned!(oid, uploader, reader, ctx);
                        async move {
                            tokio::spawn(async move {
                                let tree_for_commit =
                                    reader.read_raw_object(&entry.0).await.with_context(|| {
                                        format_err!(
                                            "Failed to fetch git tree {} for commit {}",
                                            entry.0,
                                            oid
                                        )
                                    })?;
                                let tree_bytes = tree_for_commit.clone();
                                // Upload packfile base item for given tree object and the raw Git tree
                                let packfile_item_upload = async {
                                    uploader
                                        .upload_packfile_base_item(&ctx, entry.0, tree_for_commit)
                                        .await
                                        .with_context(|| {
                                            format_err!(
                                                "Failed to upload packfile item for git tree {} for commit {}",
                                                entry.0,
                                                oid
                                            )
                                        })
                                };
                                let git_tree_upload = async {
                                    uploader
                                        .upload_object(&ctx, entry.0, tree_bytes).await
                                        .with_context(|| {
                                            format_err!("Failed to upload raw git tree {} for commit {}", entry.0, oid)
                                        })
                                };
                                try_join!(packfile_item_upload, git_tree_upload)?;
                                anyhow::Ok(())
                            })
                            .await?
                        }
                    })
                    .try_buffer_unordered(100)
                    .try_collect()
                    .await?;
                // Upload packfile base item for Git commit and the raw Git commit
                let packfile_item_upload = async {
                    uploader
                        .upload_packfile_base_item(
                            &ctx,
                            oid,
                            extracted_commit.original_commit.clone(),
                        )
                        .await
                        .with_context(|| {
                            format_err!("Failed to upload packfile item for git commit {}", oid)
                        })
                };
                let git_commit_upload = async {
                    uploader
                        .upload_object(&ctx, oid, extracted_commit.original_commit.clone())
                        .await
                        .with_context(|| format_err!("Failed to upload raw git commit {}", oid))
                };
                try_join!(packfile_item_upload, git_commit_upload)?;
                // Upload Git commit
                let (int_cs, bcs_id) = uploader
                    .generate_changeset_for_commit(
                        &ctx,
                        bonsai_parents,
                        extracted_commit.metadata,
                        file_changes,
                        dry_run,
                    )
                    .await.context("generate_changeset_for_commit")?;
                acc.write().expect("lock poisoned").insert(oid, bcs_id);

                let git_sha1 = oid_to_sha1(&oid)?;
                info!(
                    ctx.logger(),
                    "GitRepo:{} commit {} of {} - Oid:{} => Bid:{}",
                    &repo_name,
                    acc.read().expect("lock poisoned").len(),
                    nb_commits_to_import,
                    git_sha1.to_brief(),
                    bcs_id.to_brief()
                );
                Ok((int_cs, git_sha1))
            }
        })
        // Chunk together into Vec<std::result::Result<(bcs, oid), Error> >
        .chunks(prefs.concurrency)
        // Go from Vec<Result<X,Y>> -> Result<Vec<X>,Y>
        .map(|v| v.into_iter().collect::<Result<Vec<_>>>())
        .try_for_each(|v| async {
            cloned!(backfill_derivation, ctx, uploader);
            task::spawn(async move { uploader.finalize_batch(&ctx, dry_run, backfill_derivation, v).await.context("finalize_batch") }).await?
        })
        .await?;

    debug!(ctx.logger(), "Completed git import for repo {}.", repo_name);
    Ok(acc.into_inner().expect("lock poisoned"))
}

pub async fn gitimport(
    ctx: &CoreContext,
    path: &Path,
    uploader: &impl GitUploader,
    target: &GitimportTarget,
    prefs: &GitimportPreferences,
) -> Result<LinkedHashMap<ObjectId, ChangesetId>> {
    Ok(gitimport_acc(ctx, path, uploader, target, prefs)
        .await?
        .inner)
}

/// Object representing Git refs. maybe_tag_id will only
/// have a value if the ref is a tag pointing to a commit.
#[derive(Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitRef {
    pub name: Vec<u8>,
    pub maybe_tag_id: Option<ObjectId>,
}

impl GitRef {
    fn new(name: Vec<u8>) -> Self {
        Self {
            name,
            maybe_tag_id: None,
        }
    }
}

/// Read symbolic references from git
pub async fn read_symref(
    symref_name: &str,
    path: &Path,
    prefs: &GitimportPreferences,
) -> Result<GitSymbolicRefsEntry> {
    let mut command = Command::new(&prefs.git_command_path)
        .current_dir(path)
        .env_clear()
        .kill_on_drop(false)
        .stdout(Stdio::piped())
        .arg("symbolic-ref")
        .arg(symref_name)
        .spawn()
        .with_context(|| format!("failed to run git with {:?}", prefs.git_command_path))?;
    let mut stdout = BufReader::new(command.stdout.take().context("stdout not set up")?);
    let mut ref_mapping = String::new();
    stdout.read_line(&mut ref_mapping).await.with_context(|| {
        format!(
            "failed to get output of git symbolic-ref for ref {} at path {}",
            symref_name,
            path.display()
        )
    })?;
    let ref_mapping = ref_mapping.trim();
    let symref_entry = match ref_mapping.strip_prefix(BRANCH_REF_PREFIX) {
        Some(branch_name) => GitSymbolicRefsEntry::new(
            symref_name.to_string(),
            branch_name.to_string(),
            BRANCH_REF.to_string(),
        )?,
        None => match ref_mapping.strip_prefix(TAG_REF_PREFIX) {
            Some(tag_name) => GitSymbolicRefsEntry::new(
                symref_name.to_string(),
                tag_name.to_string(),
                TAG_REF.to_string(),
            )?,
            None => anyhow::bail!(
                "Unexpected ref format {} for symref {}",
                ref_mapping,
                symref_name
            ),
        },
    };
    Ok(symref_entry)
}

/// Resolve git rev using `git rev-parse --verfify`
pub async fn resolve_rev(
    rev: &str,
    path: &Path,
    prefs: &GitimportPreferences,
) -> Result<Option<ObjectId>> {
    let output = Command::new(&prefs.git_command_path)
        .current_dir(path)
        .env_clear()
        .kill_on_drop(false)
        .stdout(Stdio::piped())
        .arg("rev-parse")
        .arg("--verify")
        .arg("--end-of-options")
        .arg(rev)
        .output()
        .await
        .with_context(|| format!("failed to run git with {:?}", prefs.git_command_path))?;
    if !output.status.success() {
        return Ok(None);
    }
    let oid_str = str::from_utf8(&output.stdout)?;
    let oid_str = oid_str.trim();
    let oid: ObjectId = oid_str.parse().context("reading refs")?;
    Ok(Some(oid))
}

pub async fn read_git_refs(
    path: &Path,
    prefs: &GitimportPreferences,
) -> Result<BTreeMap<GitRef, ObjectId>> {
    let reader = GitRepoReader::new(&prefs.git_command_path, path).await?;

    let mut command = Command::new(&prefs.git_command_path)
        .current_dir(path)
        .env_clear()
        .kill_on_drop(false)
        .stdout(Stdio::piped())
        .arg("for-each-ref")
        .arg("--format=%(objectname) %(refname)")
        .spawn()
        .with_context(|| format!("failed to run git with {:?}", prefs.git_command_path))?;
    let stdout = BufReader::new(command.stdout.take().context("stdout not set up")?);
    let mut lines = stdout.lines();

    let mut refs = BTreeMap::new();

    while let Some(line) = lines
        .next_line()
        .await
        .context("git command didn't output anything")?
    {
        if let Some((oid_str, ref_name)) = line.split_once(' ') {
            let mut oid: ObjectId = oid_str.parse().context("reading refs")?;
            let mut git_ref = GitRef::new(ref_name.into());
            loop {
                let object = reader.get_object(&oid).await.with_context(|| {
                    format!("unable to read git object: {oid} for ref: {ref_name}")
                })?;
                match object.parsed {
                    Object::Tree(_) => {
                        // This happens in the Linux kernel repo, because Linus was being clever - a commit and a tree
                        // are both treeish for the purposes of things like checkout and diff.
                        break;
                    }
                    Object::Blob(_) => {
                        bail!("ref {} points to a blob", ref_name);
                    }
                    Object::Commit(_) => {
                        refs.insert(git_ref, oid);
                        break;
                    }
                    // If the ref is a tag, then we capture the object id of the tag.
                    // The loop is designed to peel the tag but we want the outermost
                    // tag object so only get the ID if we haven't already done it before.
                    Object::Tag(tag) => {
                        if git_ref.maybe_tag_id.is_none() {
                            git_ref.maybe_tag_id = Some(oid);
                        }
                        oid = tag.target;
                    }
                }
            }
        }
    }
    Ok(refs)
}

pub async fn import_tree_as_single_bonsai_changeset(
    ctx: &CoreContext,
    path: &Path,
    uploader: impl GitUploader,
    git_cs_id: ObjectId,
    prefs: &GitimportPreferences,
) -> Result<ChangesetId> {
    let reader = GitRepoReader::new(&prefs.git_command_path, path).await?;

    let sha1 = oid_to_sha1(&git_cs_id)?;

    let extracted_commit = ExtractedCommit::new(ctx, git_cs_id, &reader)
        .await
        .with_context(|| format!("While extracting {}", git_cs_id))?;

    let diff = extracted_commit.diff_root(ctx, &reader, prefs.submodules);
    let file_changes = find_file_changes(ctx, &prefs.lfs, &reader, uploader.clone(), diff).await?;

    // Before generating the corresponding changeset at Mononoke end, upload the raw git commit.
    uploader
        .upload_object(ctx, git_cs_id, extracted_commit.original_commit)
        .await
        .with_context(|| format_err!("Failed to upload raw git commit {}", git_cs_id))?;

    uploader
        .generate_changeset_for_commit(
            ctx,
            vec![],
            extracted_commit.metadata,
            file_changes,
            prefs.dry_run,
        )
        .and_then(|(cs, id)| {
            uploader
                .finalize_batch(
                    ctx,
                    prefs.dry_run,
                    prefs.backfill_derivation.clone(),
                    vec![(cs, sha1)],
                )
                .map_ok(move |_| id)
        })
        .await
}
