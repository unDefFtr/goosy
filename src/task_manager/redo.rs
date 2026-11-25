use super::{TaskId, TaskManager, exists, output_identity, reservation_path};
use crate::RenderOptions;
pub use crate::workspace::AssetRole;
use crate::workspace::{Fingerprint, InputFingerprints, Workspace};
use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};

pub struct AssetIssue {
    pub message: String,
    pub can_accept: bool,
}

struct ReviewedAsset {
    path: PathBuf,
    content: Fingerprint,
}

/// A detached copy of a task's configuration. Asset approvals belong to this draft only.
pub struct RedoDraft {
    pub options: RenderOptions,
    pub preview_interval: u32,
    source_output: PathBuf,
    original: InputFingerprints,
    offered: [Option<ReviewedAsset>; AssetRole::ALL.len()],
    accepted: [Option<ReviewedAsset>; AssetRole::ALL.len()],
}

impl TaskManager {
    pub fn redo_draft(&self, id: TaskId) -> Result<RedoDraft> {
        let (mut options, preview_interval, original) = {
            let state = self.state.lock().unwrap();
            let task = state.tasks.iter().find(|task| task.info.id == id).context("任务不存在")?;
            ensure!(task.info.state != super::TaskState::Deleting, "正在删除此任务");
            let mut options = task.record.options.clone();
            let original = Workspace::redo_inputs(&task.workspace, &mut options)?;
            (options, task.record.interval, original)
        };
        let output = available_output(&options.output)?;
        let source_output = std::mem::replace(&mut options.output, output);
        Ok(RedoDraft {
            options,
            preview_interval,
            source_output,
            original,
            offered: Default::default(),
            accepted: Default::default(),
        })
    }
}

fn available_output(source: &Path) -> Result<PathBuf> {
    let stem = source.file_stem().context("原输出路径缺少文件名")?;
    for n in 1_u64.. {
        let mut name = stem.to_owned();
        name.push("-redo");
        if n > 1 {
            name.push(format!("-{n}"));
        }
        if let Some(extension) = source.extension() {
            name.push(".");
            name.push(extension);
        }
        let candidate = source.with_file_name(name);
        if !exists(&candidate)? && !exists(&reservation_path(&candidate)?)? {
            return Ok(candidate);
        }
    }
    anyhow::bail!("无法生成可用的输出文件名")
}

impl RedoDraft {
    fn unapproved(&self, role: AssetRole, path: &Path, current: &Fingerprint) -> Option<String> {
        let index = role as usize;
        if self.original[index].as_ref() == Some(current)
            || self.accepted[index].as_ref().is_some_and(|asset| {
                asset.path == path && asset.content == *current
            })
        {
            return None;
        }
        let reason = if self.original[index].is_some() {
            "文件内容与历史指纹不一致"
        } else {
            "没有可对比的历史指纹"
        };
        Some(format!("{reason}，请重新定位或确认使用当前文件：{}", path.display()))
    }

    /// Inspect the currently selected file for one role. A moved, byte-identical original is valid.
    pub fn inspect(&mut self, role: AssetRole) -> Option<AssetIssue> {
        self.offered[role as usize] = None;
        let path = role.path(&self.options)?;
        let content = match Fingerprint::read(path) {
            Ok(content) => content,
            Err(error) => return Some(AssetIssue {
                message: format!("素材缺失或无法读取，请重新定位 {}：{error:#}", path.display()),
                can_accept: false,
            }),
        };
        let message = self.unapproved(role, path, &content)?;
        self.offered[role as usize] = Some(ReviewedAsset { path: path.to_owned(), content });
        Some(AssetIssue { message, can_accept: true })
    }

    /// Authorize precisely the path and content reported by the most recent inspection.
    pub fn accept(&mut self, role: AssetRole) -> Result<()> {
        let index = role as usize;
        let asset = self.offered[index].take().context("请先检查当前素材")?;
        ensure!(role.path(&self.options) == Some(asset.path.as_path()), "素材路径已更改，请重新检查");
        ensure!(Fingerprint::read(&asset.path)? == asset.content, "素材在检查后又发生变化，请重新检查");
        self.accepted[index] = Some(asset);
        Ok(())
    }

    /// Recheck immediately before enqueue. Workspace creation compares its own durable snapshot
    /// with the same fingerprints, closing the gap between wizard validation and task creation.
    pub fn submit(&self, manager: &TaskManager) -> Result<TaskId> {
        let output = output_identity(&self.options.output)?;
        ensure!(
            reservation_path(&output)? != reservation_path(&self.source_output)?,
            "请选择与原任务不同的输出文件"
        );
        super::check_output(&output)?;
        let mut expected = InputFingerprints::default();
        for role in AssetRole::ALL {
            if let Some(path) = role.path(&self.options) {
                let content = Fingerprint::read(path)
                    .with_context(|| format!("素材缺失或无法读取，请重新定位 {}", path.display()))?;
                if let Some(message) = self.unapproved(role, path, &content) {
                    anyhow::bail!(message);
                }
                expected[role as usize] = Some(content);
            }
        }
        manager.enqueue_with_inputs(self.options.clone(), self.preview_interval, Some(&expected))
    }
}
