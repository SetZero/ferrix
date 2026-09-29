//! `ext-workspace-v1`: the workspaces as a bar sees them.
//!
//! Every Hyprland bar draws the workspace numbers, and until this protocol
//! each one did it by reading `hyprctl` or the event socket -- which is
//! Hyprland-shaped and works nowhere else. This is the Wayland way to ask,
//! and a bar written against it works on any compositor that answers.
//!
//! # Groups
//!
//! A group is the set of workspaces one monitor can show. This compositor
//! gives each monitor one group, which is what Hyprland's per-monitor
//! workspaces are; `create_workspace` and `assign` are answered by the
//! compositor above, since making a workspace is the layout's.

use compositor_protocol::ext_workspace;
use compositor_protocol::ext_workspace::{
    ext_workspace_group_handle_v1, ext_workspace_handle_v1, ext_workspace_manager_v1,
};
use compositor_wire::{Arg, ArgType, ObjectId};

use crate::client::{Client, Event};
use crate::role::Role;

/// The version the server makes a group and a workspace handle at.
const HANDLE_VERSION: u32 = 1;

/// One workspace, as this protocol describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    /// Its number, which is what the compositor calls it.
    pub id: i64,
    /// Its name, which is the number unless it was renamed.
    pub name: String,
    /// Which monitor's group it belongs to.
    pub group: usize,
    /// Whether it is the one its monitor is showing.
    pub active: bool,
    /// Whether anything on it is asking for attention.
    pub urgent: bool,
}

/// What a client asked be done to a workspace.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WorkspaceRequest {
    /// `activate`: show it.
    Activate,
    /// `deactivate`: stop showing it.
    Deactivate,
    /// `remove`: take it away.
    Remove,
}

impl Client {
    /// Answer a request to one of this module's objects.
    pub(super) fn workspaces(&mut self, sender: ObjectId, role: Role, opcode: u16) -> bool {
        match role {
            Role::WorkspaceManager => self.workspace_manager(sender, opcode),
            Role::WorkspaceGroup => {}
            Role::WorkspaceHandle => self.workspace_handle(sender, opcode),
            _ => return false,
        }
        true
    }

    /// Drop what one of this module's objects held.
    pub(super) fn forget_workspaces(&mut self, id: ObjectId, role: Role) {
        match role {
            Role::WorkspaceManager => self.workspace_managers.retain(|held| *held != id),
            Role::WorkspaceGroup => self.workspace_groups.retain(|_, held| *held != id),
            Role::WorkspaceHandle => self.workspace_handles.retain(|_, held| *held != id),
            _ => {}
        }
    }

    /// `ext_workspace_manager_v1`: `commit` and `stop`.
    ///
    /// `commit` carries out whatever the client asked for since the last
    /// one; the requests themselves are queued as they arrive, so there is
    /// nothing to do here but say the batch has ended.
    fn workspace_manager(&mut self, sender: ObjectId, opcode: u16) {
        match opcode {
            ext_workspace_manager_v1::request::COMMIT => {
                self.events.push(Event::WorkspacesCommitted);
            }
            ext_workspace_manager_v1::request::STOP => {
                self.workspace_managers.retain(|held| *held != sender);
                let _ = self
                    .out
                    .write(sender, ext_workspace_manager_v1::event::FINISHED, &[], &[]);
            }
            _ => {}
        }
    }

    /// `ext_workspace_handle_v1`: the three that ask for something.
    fn workspace_handle(&mut self, sender: ObjectId, opcode: u16) {
        let what = match opcode {
            ext_workspace_handle_v1::request::ACTIVATE => WorkspaceRequest::Activate,
            ext_workspace_handle_v1::request::DEACTIVATE => WorkspaceRequest::Deactivate,
            ext_workspace_handle_v1::request::REMOVE => WorkspaceRequest::Remove,
            _ => return,
        };
        let Some(workspace) = self
            .workspace_handles
            .iter()
            .find(|(_, held)| **held == sender)
            .map(|(id, _)| *id)
        else {
            return;
        };
        self.events.push(Event::WorkspaceAsked { workspace, what });
    }

    /// Whether this client is watching the workspaces.
    #[must_use]
    pub fn watches_workspaces(&self) -> bool {
        !self.workspace_managers.is_empty()
    }

    /// Tell every `ext_workspace_manager_v1` what the workspaces are.
    ///
    /// One group a monitor, and every workspace in the group of the monitor
    /// it belongs to. A workspace that has gone is `removed`; one whose
    /// state changed is told again; and the whole batch ends with `done`,
    /// which is what tells a bar to draw.
    pub fn publish_workspaces(&mut self, groups: usize, workspaces: &[Workspace]) {
        if self.workspace_managers.is_empty() {
            return;
        }
        for group in 0..groups {
            if !self.workspace_groups.contains_key(&group) {
                self.make_group(group);
            }
        }
        let here: Vec<i64> = workspaces.iter().map(|workspace| workspace.id).collect();
        let gone: Vec<i64> = self
            .workspace_handles
            .keys()
            .copied()
            .filter(|id| !here.contains(id))
            .collect();
        for id in gone {
            let Some(handle) = self.workspace_handles.remove(&id) else {
                continue;
            };
            let _ = self
                .out
                .write(handle, ext_workspace_handle_v1::event::REMOVED, &[], &[]);
            let _ = self.workspace_told.remove(&id);
        }
        for workspace in workspaces {
            if !self.workspace_handles.contains_key(&workspace.id) {
                self.make_workspace(workspace);
            }
            if self.workspace_told.get(&workspace.id) == Some(workspace) {
                continue;
            }
            self.tell_workspace(workspace);
            let _ = self.workspace_told.insert(workspace.id, workspace.clone());
        }
        let managers = self.workspace_managers.clone();
        for manager in managers {
            let _ = self
                .out
                .write(manager, ext_workspace_manager_v1::event::DONE, &[], &[]);
        }
    }

    /// One monitor's group.
    fn make_group(&mut self, group: usize) {
        let managers = self.workspace_managers.clone();
        let Ok(id) = self.objects.create(
            &ext_workspace::EXT_WORKSPACE_GROUP_HANDLE_V1,
            HANDLE_VERSION,
            Role::WorkspaceGroup,
        ) else {
            return;
        };
        for manager in &managers {
            let _ = self.out.write(
                *manager,
                ext_workspace_manager_v1::event::WORKSPACE_GROUP,
                &[ArgType::NewId],
                &[Arg::NewId(id)],
            );
        }
        // This compositor makes a workspace when a person asks for one by
        // number, which is `create_workspace`'s capability.
        let _ = self.out.write(
            id,
            ext_workspace_group_handle_v1::event::CAPABILITIES,
            &[ArgType::Uint],
            &[Arg::Uint(
                ext_workspace_group_handle_v1::group_capabilities::CREATE_WORKSPACE,
            )],
        );
        if let Some(output) = self.output_objects.iter().find(|(_, at)| **at == group) {
            let _ = self.out.write(
                id,
                ext_workspace_group_handle_v1::event::OUTPUT_ENTER,
                &[ArgType::Object { nullable: false }],
                &[Arg::Object(*output.0)],
            );
        }
        let _ = self.workspace_groups.insert(group, id);
    }

    /// One workspace, and which group it is in.
    fn make_workspace(&mut self, workspace: &Workspace) {
        let managers = self.workspace_managers.clone();
        let Ok(id) = self.objects.create(
            &ext_workspace::EXT_WORKSPACE_HANDLE_V1,
            HANDLE_VERSION,
            Role::WorkspaceHandle,
        ) else {
            return;
        };
        for manager in &managers {
            let _ = self.out.write(
                *manager,
                ext_workspace_manager_v1::event::WORKSPACE,
                &[ArgType::NewId],
                &[Arg::NewId(id)],
            );
        }
        if let Some(group) = self.workspace_groups.get(&workspace.group).copied() {
            let _ = self.out.write(
                group,
                ext_workspace_group_handle_v1::event::WORKSPACE_ENTER,
                &[ArgType::Object { nullable: false }],
                &[Arg::Object(id)],
            );
        }
        let _ = self.out.write(
            id,
            ext_workspace_handle_v1::event::CAPABILITIES,
            &[ArgType::Uint],
            &[Arg::Uint(
                ext_workspace_handle_v1::workspace_capabilities::ACTIVATE
                    | ext_workspace_handle_v1::workspace_capabilities::DEACTIVATE,
            )],
        );
        let _ = self.workspace_handles.insert(workspace.id, id);
    }

    /// What one workspace is called and what state it is in.
    fn tell_workspace(&mut self, workspace: &Workspace) {
        let Some(id) = self.workspace_handles.get(&workspace.id).copied() else {
            return;
        };
        let number = workspace.id.to_string();
        for (event, text) in [
            (ext_workspace_handle_v1::event::ID, number),
            (ext_workspace_handle_v1::event::NAME, workspace.name.clone()),
        ] {
            let _ = self.out.write(
                id,
                event,
                &[ArgType::Str { nullable: false }],
                &[Arg::Str(Some(&text))],
            );
        }
        let mut state = 0;
        if workspace.active {
            state |= ext_workspace_handle_v1::state::ACTIVE;
        }
        if workspace.urgent {
            state |= ext_workspace_handle_v1::state::URGENT;
        }
        let _ = self.out.write(
            id,
            ext_workspace_handle_v1::event::STATE,
            &[ArgType::Uint],
            &[Arg::Uint(state)],
        );
    }
}
