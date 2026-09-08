//! Installable agent skills shipped with the binary.
//!
//! These are the same skill files this repo uses on itself (under
//! `.claude/skills/`), compiled in via `include_str!` so `skills_install` can
//! write them into any target directory the user chooses (their global agent
//! skill directory, a project's skill directory, or anywhere else) —
//! no network access, no separate download step. Installed skill directories
//! are always named `varde-code-<pack name>` so they're recognizable and
//! never collide with a user's own skills.

use std::io;
use std::path::{Path, PathBuf};

/// One file within a skill pack, relative to the pack's own directory.
pub struct SkillFile {
    pub rel_path: &'static str,
    pub contents: &'static str,
}

/// A skill directory shippable via `skills_install`.
pub struct SkillPack {
    /// Bare name, e.g. `"rule-authoring"`. Installed as `varde-code-<name>`.
    pub name: &'static str,
    pub files: &'static [SkillFile],
}

pub const SKILL_PACKS: &[SkillPack] = &[
    SkillPack {
        name: "rule-authoring",
        files: &[
            SkillFile {
                rel_path: "SKILL.md",
                contents: include_str!(
                    "../../../.claude/skills/varde-code-rule-authoring/SKILL.md"
                ),
            },
            SkillFile {
                rel_path: "references/RULE-FORMAT.md",
                contents: include_str!(
                    "../../../.claude/skills/varde-code-rule-authoring/references/RULE-FORMAT.md"
                ),
            },
        ],
    },
    SkillPack {
        name: "rule-scan-triage",
        files: &[SkillFile {
            rel_path: "SKILL.md",
            contents: include_str!("../../../.claude/skills/varde-code-rule-scan-triage/SKILL.md"),
        }],
    },
    SkillPack {
        name: "codebase-navigation",
        files: &[
            SkillFile {
                rel_path: "SKILL.md",
                contents: include_str!(
                    "../../../.claude/skills/varde-code-codebase-navigation/SKILL.md"
                ),
            },
            SkillFile {
                rel_path: "agents/openai.yaml",
                contents: include_str!(
                    "../../../.claude/skills/varde-code-codebase-navigation/agents/openai.yaml"
                ),
            },
        ],
    },
];

/// The directory name a pack installs as — always `varde-code-<name>`.
pub fn install_dir_name(pack_name: &str) -> String {
    format!("varde-code-{pack_name}")
}

/// Outcome of installing a single file from a skill pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallResult {
    pub pack_name: String,
    pub path: PathBuf,
    pub written: bool,
    /// `true` when the file already existed and `force` was false, so it was
    /// left untouched.
    pub skipped_existing: bool,
}

/// Write every skill pack into `target_dir`, one `varde-code-<name>/`
/// subdirectory per pack. Existing files are left untouched unless `force`
/// is true — safe to re-run after an upgrade without clobbering local edits.
pub fn install_skills(target_dir: &Path, force: bool) -> io::Result<Vec<InstallResult>> {
    let mut results = Vec::new();
    for pack in SKILL_PACKS {
        let pack_dir = target_dir.join(install_dir_name(pack.name));
        for file in pack.files {
            let path = pack_dir.join(file.rel_path);
            let exists = path.exists();
            if exists && !force {
                results.push(InstallResult {
                    pack_name: pack.name.to_string(),
                    path,
                    written: false,
                    skipped_existing: true,
                });
                continue;
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, file.contents)?;
            results.push(InstallResult {
                pack_name: pack.name.to_string(),
                path,
                written: true,
                skipped_existing: false,
            });
        }
    }
    Ok(results)
}

/// Outcome of removing a single previously installed skill pack file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveResult {
    pub pack_name: String,
    pub path: PathBuf,
    pub removed: bool,
    /// `true` when the file's contents no longer match the shipped skill
    /// (locally edited) and `force` was false, so it was left alone.
    pub skipped_modified: bool,
}

/// Delete previously installed `varde-code-<name>/` skill directories from
/// `target_dir`. Only files matching a shipped pack are touched; anything
/// else under `target_dir` (including unrelated `varde-code-*` dirs a user
/// created themselves) is left alone. A file edited since install is left in
/// place unless `force` is true. Empty pack directories are removed after
/// their files are cleared.
pub fn remove_skills(target_dir: &Path, force: bool) -> io::Result<Vec<RemoveResult>> {
    let mut results = Vec::new();
    for pack in SKILL_PACKS {
        let pack_dir = target_dir.join(install_dir_name(pack.name));
        for file in pack.files {
            let path = pack_dir.join(file.rel_path);
            if !path.exists() {
                continue;
            }
            let on_disk = std::fs::read_to_string(&path)?;
            let modified = on_disk != file.contents;
            if modified && !force {
                results.push(RemoveResult {
                    pack_name: pack.name.to_string(),
                    path,
                    removed: false,
                    skipped_modified: true,
                });
                continue;
            }
            std::fs::remove_file(&path)?;
            results.push(RemoveResult {
                pack_name: pack.name.to_string(),
                path,
                removed: true,
                skipped_modified: false,
            });
        }
        // Best-effort cleanup: only succeeds once the directory (and any
        // subdirectories like `references/`) is actually empty.
        let _ = remove_empty_dirs(&pack_dir);
    }
    Ok(results)
}

fn remove_empty_dirs(dir: &Path) -> io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let _ = remove_empty_dirs(&entry.path());
        }
    }
    if std::fs::read_dir(dir)?.next().is_none() {
        std::fs::remove_dir(dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tempdir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("varde-skills-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir creates");
        dir
    }

    #[test]
    fn install_then_remove_round_trips() {
        let dir = tempdir("round-trip");

        let installed = install_skills(&dir, false).expect("install succeeds");
        assert!(!installed.is_empty());
        assert!(installed.iter().all(|r| r.written && !r.skipped_existing));
        for pack in SKILL_PACKS {
            assert!(dir.join(install_dir_name(pack.name)).is_dir());
        }
        assert!(dir.join("varde-code-rule-authoring/SKILL.md").exists());
        assert!(
            dir.join("varde-code-rule-authoring/references/RULE-FORMAT.md")
                .exists()
        );
        assert!(
            dir.join("varde-code-codebase-navigation/agents/openai.yaml")
                .exists()
        );
        let navigation_ui =
            fs::read_to_string(dir.join("varde-code-codebase-navigation/agents/openai.yaml"))
                .expect("navigation UI metadata reads");
        assert!(navigation_ui.contains("$varde-code-codebase-navigation"));

        let removed = remove_skills(&dir, false).expect("remove succeeds");
        assert_eq!(removed.len(), installed.len());
        assert!(removed.iter().all(|r| r.removed && !r.skipped_modified));
        for pack in SKILL_PACKS {
            assert!(
                !dir.join(install_dir_name(pack.name)).exists(),
                "pack dir cleaned up"
            );
        }
    }

    #[test]
    fn install_is_idempotent_without_force() {
        let dir = tempdir("idempotent");
        install_skills(&dir, false).expect("first install succeeds");
        let second = install_skills(&dir, false).expect("second install succeeds");
        assert!(second.iter().all(|r| !r.written && r.skipped_existing));
    }

    #[test]
    fn remove_skips_locally_modified_files_unless_forced() {
        let dir = tempdir("remove-modified");
        install_skills(&dir, false).expect("install succeeds");
        let edited = dir.join("varde-code-rule-scan-triage/SKILL.md");
        fs::write(&edited, "# locally customized\n").expect("edit writes");

        let removed = remove_skills(&dir, false).expect("remove succeeds");
        let entry = removed
            .iter()
            .find(|r| r.path == edited)
            .expect("entry present");
        assert!(!entry.removed && entry.skipped_modified);
        assert!(edited.exists());

        let forced = remove_skills(&dir, true).expect("forced remove succeeds");
        let entry = forced
            .iter()
            .find(|r| r.path == edited)
            .expect("entry present");
        assert!(entry.removed && !entry.skipped_modified);
        assert!(!edited.exists());
    }

    #[test]
    fn skill_names_match_portable_install_directories() {
        for pack in SKILL_PACKS {
            let skill = pack
                .files
                .iter()
                .find(|file| file.rel_path == "SKILL.md")
                .expect("every pack has SKILL.md");
            let name = format!("name: {}", install_dir_name(pack.name));
            assert!(
                skill.contents.contains(&name),
                "{} must declare {name}",
                pack.name
            );
        }
    }
}
