/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::HashMap;
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::anyhow;
use anyhow::Context;
use anyhow::Error;
use anyhow::Result;
use blobstore::Storable;
use changesets_creation::save_changesets;
use context::CoreContext;
use fsnodes::RootFsnodeId;
use futures::future;
use futures::stream;
use futures::stream::Stream;
use futures::stream::StreamExt;
use futures::stream::TryStreamExt;
use git_types::ObjectKind;
use itertools::Itertools;
use manifest::bonsai_diff;
use manifest::BonsaiDiffFileChange;
use manifest::Entry;
use manifest::ManifestOps;
use mononoke_types::hash::GitSha1;
use mononoke_types::hash::RichGitSha1;
use mononoke_types::BlobstoreValue;
use mononoke_types::BonsaiChangesetMut;
use mononoke_types::ChangesetId;
use mononoke_types::ContentId;
use mononoke_types::FileChange;
use mononoke_types::FileContents;
use mononoke_types::FileType;
use mononoke_types::FsnodeId;
use mononoke_types::GitLfs;
use mononoke_types::MPathElement;
use mononoke_types::NonRootMPath;
use movers::Mover;
use repo_blobstore::RepoBlobstoreArc;
use repo_derived_data::RepoDerivedDataRef;
use sorted_vector_map::SortedVectorMap;

use crate::git_submodules::expand::SubmoduleExpansionData;
use crate::git_submodules::expand::SubmodulePath;
use crate::git_submodules::in_memory_repo::InMemoryRepo;
use crate::reporting::log_warning;
use crate::types::Repo;

/// Get the git hash from a submodule file, which represents the commit from the
/// given submodule that the source repo depends on at that revision.
pub(crate) async fn get_git_hash_from_submodule_file<'a, R: Repo>(
    ctx: &'a CoreContext,
    repo: &'a R,
    submodule_file_content_id: ContentId,
) -> Result<GitSha1> {
    let blobstore = repo.repo_blobstore_arc();

    let bytes = filestore::fetch_concat_exact(&blobstore, ctx, submodule_file_content_id, 20)
        .await
        .with_context(
            || "Failed to fetch content of file containing the submodule's git commit hash",
        )?;

    let git_submodule_hash = RichGitSha1::from_bytes(&bytes, ObjectKind::Commit.as_str(), 0)?;
    let git_submodule_sha1 = git_submodule_hash.sha1();

    anyhow::Ok(git_submodule_sha1)
}

/// Get the git hash from a submodule file, which represents the commit from the
/// given submodule that the source repo depends on at that revision.
pub(crate) async fn git_hash_from_submodule_metadata_file<'a>(
    ctx: &'a CoreContext,
    large_repo: &'a InMemoryRepo,
    submodule_file_content_id: ContentId,
) -> Result<GitSha1> {
    let bytes = filestore::fetch_concat_exact(large_repo.repo_blobstore(), ctx, submodule_file_content_id, 40)
      .await
      .with_context(|| {
          format!(
              "Failed to fetch content from content id {} file containing the submodule's git commit hash",
              &submodule_file_content_id
          )
      })?;

    let git_hash_string = std::str::from_utf8(bytes.as_ref())?;
    let git_sha1 = GitSha1::from_str(git_hash_string)?;

    anyhow::Ok(git_sha1)
}

pub(crate) fn get_submodule_repo<'a, 'b, R: Repo>(
    sm_path: &'a SubmodulePath,
    submodule_deps: &'b HashMap<NonRootMPath, Arc<R>>,
) -> Result<&'b R> {
    let repo_arc = submodule_deps
        .get(&sm_path.0)
        .ok_or_else(|| anyhow!("Mononoke repo from submodule {} not available", sm_path.0))?;

    Ok(repo_arc.as_ref())
}

/// Returns true if the given path is a git submodule.
pub(crate) async fn is_path_git_submodule(
    ctx: &CoreContext,
    repo: &impl Repo,
    changeset: ChangesetId,
    path: &NonRootMPath,
) -> Result<bool, Error> {
    Ok(get_submodule_file_content_id(ctx, repo, changeset, path)
        .await?
        .is_some())
}

pub(crate) fn x_repo_submodule_metadata_file_basename<S: std::fmt::Display>(
    submodule_basename: &S,
    x_repo_submodule_metadata_file_prefix: &str,
) -> Result<MPathElement> {
    MPathElement::new(
        format!(".{x_repo_submodule_metadata_file_prefix}-{submodule_basename}")
            .to_string()
            .into_bytes(),
    )
}
/// Builds the full path of the x-repo submodule metadata file for a given
/// submodule.
pub(crate) fn get_x_repo_submodule_metadata_file_path(
    submodule_file_path: &SubmodulePath,
    // Prefix used to generate the metadata file basename. Obtained from
    // the small repo sync config.
    x_repo_submodule_metadata_file_prefix: &str,
) -> Result<NonRootMPath> {
    let (mb_sm_parent_dir, sm_basename) = submodule_file_path.0.split_dirname();

    let x_repo_sm_metadata_file = x_repo_submodule_metadata_file_basename(
        &sm_basename,
        x_repo_submodule_metadata_file_prefix,
    )?;

    let x_repo_sm_metadata_path = match mb_sm_parent_dir {
        Some(sm_parent_dir) => sm_parent_dir.join(&x_repo_sm_metadata_file),
        None => x_repo_sm_metadata_file.into(),
    };
    Ok(x_repo_sm_metadata_path)
}

// Returns the differences between a submodule commit and its parents.
pub(crate) async fn submodule_diff(
    ctx: &CoreContext,
    sm_repo: &impl Repo,
    cs_id: ChangesetId,
    parents: Vec<ChangesetId>,
) -> Result<impl Stream<Item = Result<BonsaiDiffFileChange<(ContentId, u64)>>>> {
    let fsnode_id = sm_repo
        .repo_derived_data()
        .derive::<RootFsnodeId>(ctx, cs_id)
        .await
        .with_context(|| format!("Failed to get fsnode id form changeset id {}", cs_id))?
        .into_fsnode_id();

    let parent_fsnode_ids = stream::iter(parents)
        .then(|parent_cs_id| async move {
            anyhow::Ok(
                sm_repo
                    .repo_derived_data()
                    .derive::<RootFsnodeId>(ctx, parent_cs_id)
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to get parent's fsnode id from its changeset id: {}",
                            parent_cs_id
                        )
                    })?
                    .into_fsnode_id(),
            )
        })
        .try_collect::<HashSet<_>>()
        .await?;

    Ok(bonsai_diff(
        ctx.clone(),
        sm_repo.repo_blobstore_arc().clone(),
        fsnode_id,
        parent_fsnode_ids,
    ))
}

/// Returns the content id of the given path if it is a submodule file.
pub(crate) async fn get_submodule_file_content_id(
    ctx: &CoreContext,
    repo: &impl Repo,
    cs_id: ChangesetId,
    path: &NonRootMPath,
) -> Result<Option<ContentId>> {
    content_id_of_file_with_type(ctx, repo, cs_id, path, FileType::GitSubmodule).await
}

/// Returns the content id of a file at a given path if it was os a specific
/// file type.
pub(crate) async fn content_id_of_file_with_type<R>(
    ctx: &CoreContext,
    repo: &R,
    cs_id: ChangesetId,
    path: &NonRootMPath,
    expected_file_type: FileType,
) -> Result<Option<ContentId>>
where
    R: RepoDerivedDataRef + RepoBlobstoreArc,
{
    let fsnode_id = repo
        .repo_derived_data()
        .derive::<RootFsnodeId>(ctx, cs_id)
        .await?
        .into_fsnode_id();

    let entry = fsnode_id
        .find_entry(ctx.clone(), repo.repo_blobstore_arc(), path.clone().into())
        .await?;

    match entry {
        Some(Entry::Leaf(file)) if *file.file_type() == expected_file_type => {
            Ok(Some(file.content_id().clone()))
        }
        _ => Ok(None),
    }
}

pub(crate) async fn list_non_submodule_files_under<R>(
    ctx: &CoreContext,
    repo: &R,
    cs_id: ChangesetId,
    submodule_path: SubmodulePath,
) -> Result<impl Stream<Item = Result<NonRootMPath>>>
where
    R: RepoDerivedDataRef + RepoBlobstoreArc,
{
    let fsnode_id = repo
        .repo_derived_data()
        .derive::<RootFsnodeId>(ctx, cs_id)
        .await?
        .into_fsnode_id();

    Ok(fsnode_id
        .list_leaf_entries_under(
            ctx.clone(),
            repo.repo_blobstore_arc(),
            vec![submodule_path.0],
        )
        .try_filter_map(|(path, fsnode_file)| {
            future::ready(Ok(
                (*fsnode_file.file_type() != FileType::GitSubmodule).then_some(path)
            ))
        }))
}

/// Gets the root directory's fsnode id from a submodule commit provided as
/// as a git hash. This is used for working copy validation of submodule
/// expansion.
pub(crate) async fn root_fsnode_id_from_submodule_git_commit(
    ctx: &CoreContext,
    repo: &impl Repo,
    git_hash: GitSha1,
    dangling_submodule_pointers: &[GitSha1],
) -> Result<FsnodeId> {
    let cs_id =
        get_submodule_bonsai_changeset_id(ctx, repo, git_hash, dangling_submodule_pointers).await?;
    let submodule_root_fsnode_id: RootFsnodeId = repo
        .repo_derived_data()
        .derive::<RootFsnodeId>(ctx, cs_id)
        .await?;

    Ok(submodule_root_fsnode_id.into_fsnode_id())
}

/// Build a new submodule dependency map to expand/validate recursive submodules
/// under a given submodule.
/// It removes the path of the given submodule from all the entries that are
/// under it and ignores the ones that aren't.
pub(crate) fn build_recursive_submodule_deps<R: Repo>(
    submodule_deps: &HashMap<NonRootMPath, Arc<R>>,
    submodule_path: &NonRootMPath,
) -> HashMap<NonRootMPath, Arc<R>> {
    let rec_small_repo_deps: HashMap<NonRootMPath, Arc<R>> = submodule_deps
        .iter()
        .filter_map(|(p, repo)| {
            p.remove_prefix_component(submodule_path)
                .map(|relative_p| (relative_p, repo.clone()))
        })
        .collect();

    rec_small_repo_deps
}

/// Returns the submodule expansions affected by a large repo changeset.
///
/// This could happen by directly modifying the submodule's expansion or its
/// metadata file.
pub(crate) fn get_submodule_expansions_affected<'a, R: Repo>(
    sm_exp_data: &SubmoduleExpansionData<'a, R>,
    // Bonsai from the large repo
    bonsai: &BonsaiChangesetMut,
    mover: Mover,
) -> Result<Vec<NonRootMPath>> {
    let submodules_affected = sm_exp_data
        .submodule_deps
        .iter()
        .map(|(submodule_path, _)| {
            // Get the submodule's metadata file path
            let metadata_file_path = get_x_repo_submodule_metadata_file_path(
                &SubmodulePath(submodule_path.clone()),
                sm_exp_data.x_repo_submodule_metadata_file_prefix,
            )?;

            let submodule_expansion_changed = bonsai
                .file_changes
                .iter()
                .map(|(p, _)| mover(p))
                .filter_map(Result::transpose)
                .process_results(|mut iter| {
                    iter.any(|small_repo_path| {
                        // File modified expansion directly
                        submodule_path.is_prefix_of(&small_repo_path)
                        // or the submodule's metadata file
                        || small_repo_path == metadata_file_path
                    })
                })?;

            if submodule_expansion_changed {
                return anyhow::Ok(Some(submodule_path.clone()));
            };

            Ok(None)
        })
        .filter_map(Result::transpose)
        .collect::<Result<Vec<_>>>()?;

    Ok(submodules_affected)
}

/// Gets the bonsai changeset id from a given git commit hash from the submodule
/// repo.
///
/// If the bonsai is not found in the bonsai git mapping, this function will
/// check the list of known dangling submodule pointers associated with the
/// small repo.
/// If the provided git commit is not there, it will crash.
///
/// If it's there, it will create a commit in the submodule repo containing a
/// single README file informing that it represents a dangling submodule pointer
/// and will return this new commit's changeset id.
pub(crate) async fn get_submodule_bonsai_changeset_id<R: Repo>(
    ctx: &CoreContext,
    submodule_repo: &R,
    git_submodule_sha1: GitSha1,
    dangling_submodule_pointers: &[GitSha1],
) -> Result<ChangesetId> {
    let mb_cs_id = submodule_repo
        .bonsai_git_mapping()
        .get_bonsai_from_git_sha1(ctx, git_submodule_sha1)
        .await?;

    if let Some(cs_id) = mb_cs_id {
        return Ok(cs_id);
    };

    if !dangling_submodule_pointers.contains(&git_submodule_sha1) {
        // Not a known dangling pointer, so it's an unexpected failure
        return Err(anyhow!(
            "Failed to get changeset id from git submodule commit hash {} in repo {}",
            &git_submodule_sha1,
            &submodule_repo.repo_identity().name()
        ));
    };

    // At this point, we know that the submodule commit hash is a dangling
    // pointer, so we create a commit in the submodule repo containing a text
    // file and return that as the commit to be expanded.
    log_warning(
        ctx,
        format!(
            "Expanding dangling submodule pointer {} from submodule repo {}",
            git_submodule_sha1,
            submodule_repo.repo_identity().name()
        ),
    );

    let exp_bonsai_cs_id =
        create_bonsai_for_dangling_submodule_pointer(git_submodule_sha1, submodule_repo, ctx)
            .await?;

    Ok(exp_bonsai_cs_id)
}

/// Create and upload a bonsai to the submodule repo to represent the dangling
/// submodule pointer that's being expanded.
async fn create_bonsai_for_dangling_submodule_pointer<R: Repo>(
    git_submodule_sha1: GitSha1,
    submodule_repo: &R,
    ctx: &CoreContext,
) -> Result<ChangesetId> {
    let readme_file_content = FileContents::new_bytes(format!(
        "This is the expansion of a known dangling submodule pointer {}. This commit doesn't exist in the repo {}",
        git_submodule_sha1,
        submodule_repo.repo_identity().name()
    ));
    let readme_file_size = readme_file_content.size();
    let readme_file_content_id = readme_file_content
        .into_blob()
        .store(ctx, submodule_repo.repo_blobstore())
        .await?;
    let dangling_expansion_file_change = FileChange::tracked(
        readme_file_content_id,
        FileType::Regular,
        readme_file_size,
        None,
        GitLfs::FullContent,
    );
    let file_changes: SortedVectorMap<NonRootMPath, FileChange> = vec![(
        NonRootMPath::new("README.TXT")?,
        dangling_expansion_file_change,
    )]
    .into_iter()
    .collect();
    let commit_msg = format!(
        "The git commit {} didn't exist in the submodule repo {}, so it's snapshot couldn't be created.",
        git_submodule_sha1,
        submodule_repo.repo_identity().name()
    );
    let exp_bonsai_mut = BonsaiChangesetMut {
        parents: vec![],
        message: commit_msg,
        file_changes,
        ..Default::default()
    };
    let exp_bonsai = exp_bonsai_mut.freeze()?;
    let exp_bonsai_cs_id = exp_bonsai.get_changeset_id();

    save_changesets(ctx, submodule_repo, vec![exp_bonsai])
        .await
        .context("Failed to save bonsai for dangling submodule pointer expansion")?;

    Ok(exp_bonsai_cs_id)
}
