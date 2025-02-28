/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::HashMap;
use std::ops::BitOr;

use anyhow::anyhow;
use anyhow::Result;
use manifest_tree::TreeEntry as ManifestTreeEntry;
use types::HgId;
use types::Parents;

use crate::scmstore::tree::types::AuxData;
use crate::scmstore::tree::types::LazyTree;
use crate::scmstore::tree::types::TreeAttributes;
use crate::scmstore::value::StoreValue;

#[derive(Debug, Default)]
pub struct StoreTree {
    pub(crate) content: Option<LazyTree>,
    pub(crate) parents: Option<Parents>,
}

impl StoreTree {
    pub fn manifest_tree_entry(&mut self) -> Result<ManifestTreeEntry> {
        self.content
            .as_mut()
            .ok_or_else(|| anyhow!("no content available"))?
            .manifest_tree_entry()
    }

    pub fn aux_data(&self) -> Result<HashMap<HgId, AuxData>> {
        Ok(self
            .content
            .as_ref()
            .ok_or_else(|| anyhow!("no content available"))?
            .aux_data())
    }
}

impl StoreValue for StoreTree {
    type Attrs = TreeAttributes;

    /// Returns which attributes are present in this StoreTree
    fn attrs(&self) -> TreeAttributes {
        TreeAttributes {
            content: self.content.is_some(),
            parents: self.parents.is_some(),
        }
    }

    /// Return a StoreTree with only the specified subset of attributes
    fn mask(self, attrs: TreeAttributes) -> Self {
        StoreTree {
            content: if attrs.content { self.content } else { None },
            parents: if attrs.parents { self.parents } else { None },
        }
    }
}

impl BitOr for StoreTree {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        StoreTree {
            content: self.content.or(rhs.content),
            parents: self.parents.or(rhs.parents),
        }
    }
}

impl From<LazyTree> for StoreTree {
    fn from(v: LazyTree) -> Self {
        let parents = v.parents();
        StoreTree {
            content: Some(v),
            parents,
        }
    }
}
