//! Bookmarks select fresh admitted report entries. They never supply source.
use super::*;

impl UiModel {
    pub(super) fn workspace_snapshot(&self) -> Option<WorkspaceSnapshot> {
        self.catalog.as_ref()?;
        Some(WorkspaceSnapshot {
            chat: self.dormant_chat.clone().unwrap_or_default(),
            mail: None,
            workbench: self.workbench_bookmark(),
            workbench_open: self.workbench,
        })
    }

    pub(super) fn has_workspace_work(&self) -> bool {
        self.catalog.is_some() && (self.workbench_seen || self.agents.has_workspace_work())
    }

    fn workbench_bookmark(&self) -> Option<WorkbenchBookmark> {
        if !self.workbench_seen {
            return None;
        }
        if let Some(bookmark) = &self.workbench_recovery {
            return Some(bookmark.clone());
        }
        let mut bookmark = WorkbenchBookmark {
            node_key: self.selected_node()?.key.clone(),
            project: self.project.clone(),
            entry: self
                .report
                .as_ref()
                .and_then(|report| self.entry_bookmark(report)),
        };
        if !bookmark.is_valid() {
            if let Some(entry) = &mut bookmark.entry {
                entry.declaration.clear();
            }
            if !bookmark.is_valid() {
                bookmark.entry = None;
            }
        }
        Some(bookmark)
    }

    fn entry_bookmark(&self, report: &ReportView) -> Option<WorkbenchEntryBookmark> {
        let mut entry = report.entries.get(self.selected_entry? as usize)?;
        let kind = entry.kind;
        let mut declaration = Vec::new();
        let mut selected = self.selected_declaration;
        while let Some(id) = selected {
            let item = &entry.declarations[id as usize];
            declaration.push(DeclarationIdentity {
                kind: item.kind,
                name: item.name.clone(),
                module_inline: item.module_inline,
                implementation: item.implementation.clone(),
            });
            selected = item.parent;
        }
        declaration.reverse();
        let mut path = Vec::new();
        while let Some(parent) = entry.parent {
            path.push(entry.name.clone());
            entry = &report.entries[parent as usize];
        }
        path.reverse();
        (!path.is_empty()).then_some(WorkbenchEntryBookmark {
            path,
            kind,
            declaration,
        })
    }

    pub(super) fn restore_workbench(
        &mut self,
        bookmark: Option<WorkbenchBookmark>,
        open: bool,
        now: u64,
        effects: &mut Vec<UiEffect>,
    ) {
        self.workbench = open;
        self.workbench_seen = open;
        let Some(bookmark) = bookmark else { return };
        let Some(catalog) = &self.catalog else { return };
        let allowed = catalog.nodes.iter().any(|node| {
            node.key == bookmark.node_key
                && (node
                    .projects
                    .iter()
                    .any(|project| project.alias == bookmark.project)
                    || (node.projects.is_empty() && bookmark.project.is_empty()))
        });
        if !allowed {
            return;
        }
        if self.pending.as_ref().is_some_and(|request| {
            request.node_key == bookmark.node_key && request.project == bookmark.project
        }) {
            self.workbench_seen = true;
            self.workbench_recovery = Some(bookmark);
            return;
        }
        if now
            .checked_add(catalog.timeout_ms)
            .is_none_or(|expiry| expiry > MAX_JS_INTEGER)
            || self.sequence == u64::MAX
            || self.revision >= u64::MAX - 1
        {
            return;
        }
        if self.pending.is_some() || bookmark.project.is_empty() {
            if self.invalidate(effects).is_err() {
                return;
            }
            self.phase = Phase::Idle;
        }
        self.node.clone_from(&bookmark.node_key);
        self.project.clone_from(&bookmark.project);
        self.workbench_seen = true;
        if self.project.is_empty() {
            self.message = "Connected. No project is available on this node.".into();
        } else if self.begin_inspection(now, effects).is_ok() {
            self.workbench_recovery = Some(bookmark);
        }
    }

    pub(super) fn resolve_workbench_recovery(&mut self, now: u64, effects: &mut Vec<UiEffect>) {
        let Some(bookmark) = self.workbench_recovery.take() else {
            return;
        };
        let (Some(report), Some(entry)) = (&self.report, bookmark.entry) else {
            return;
        };
        let Some(id) = find_entry(report, &entry) else {
            return;
        };
        let declaration = find_declaration(&report.entries[id as usize], &entry.declaration);
        if self.navigate_entry(id, now, effects).is_err() {
            return;
        }
        self.selected_declaration = declaration;
        let Some(report) = &self.report else { return };
        let mut parent = report.entries[id as usize].parent;
        while let Some(ancestor) = parent {
            self.expanded.insert(ancestor);
            parent = report.entries[ancestor as usize].parent;
        }
    }
}

fn find_entry(report: &ReportView, bookmark: &WorkbenchEntryBookmark) -> Option<u32> {
    let mut parent = 0;
    for name in &bookmark.path {
        let mut matches = report
            .entries
            .iter()
            .filter(|entry| entry.parent == Some(parent) && entry.name == *name);
        let found = matches.next()?.id;
        if matches.next().is_some() {
            return None;
        }
        parent = found;
    }
    (report.entries[parent as usize].kind == bookmark.kind).then_some(parent)
}

fn find_declaration(entry: &EntryView, bookmark: &[DeclarationIdentity]) -> Option<u32> {
    let mut parent = None;
    for identity in bookmark {
        let mut matches = entry.declarations.iter().filter(|item| {
            item.parent == parent
                && item.kind == identity.kind
                && item.name == identity.name
                && item.module_inline == identity.module_inline
                && item.implementation == identity.implementation
        });
        let found = matches.next()?.id;
        if matches.next().is_some() {
            return None;
        }
        parent = Some(found);
    }
    parent
}
