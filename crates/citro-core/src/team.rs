//! Team rules that both the hub and the app understand: the roles of a team, and who may use a group's profiles.

use serde::{Deserialize, Serialize};

/// A role inside one team. The derived order is the privilege order: `Viewer < Member < Admin < Owner`.
///
/// * `Viewer` reads (profile list without secrets, groups, logs, CSV) but cannot open, edit or create anything.
/// * `Member` opens profiles (takes locks, syncs data), creates and edits profiles and groups.
/// * `Admin` additionally deletes profiles and groups, breaks locks, manages members (but not owners), sees secrets,
///   and sets who may use a group.
/// * `Owner` additionally manages owners; a team always keeps at least one. Owners always see and open everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TeamRole {
    #[default]
    Viewer,
    Member,
    Admin,
    Owner,
}

impl TeamRole {
    pub const ALL: [TeamRole; 4] = [TeamRole::Viewer, TeamRole::Member, TeamRole::Admin, TeamRole::Owner];

    pub fn as_str(self) -> &'static str {
        match self {
            TeamRole::Viewer => "viewer",
            TeamRole::Member => "member",
            TeamRole::Admin => "admin",
            TeamRole::Owner => "owner",
        }
    }
}

impl std::str::FromStr for TeamRole {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "viewer" => Ok(TeamRole::Viewer),
            "member" => Ok(TeamRole::Member),
            "admin" => Ok(TeamRole::Admin),
            "owner" => Ok(TeamRole::Owner),
            other => Err(format!("unknown team role `{other}` (expected viewer, member, admin or owner)")),
        }
    }
}

/// Who may use the profiles of a group. The hub enforces it: a profile someone may not see is not sent to them at
/// all, and one they may not open cannot be locked, edited or have its data read by them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupAccess {
    /// The lowest role that sees the group's profiles.
    pub see: TeamRole,
    /// The lowest role that opens and edits them (never lower than member: viewers open nothing).
    pub open: TeamRole,
    /// User ids that see and open them whatever their role (a viewer among them still only looks).
    pub people: Vec<String>,
}

impl Default for GroupAccess {
    fn default() -> Self {
        GroupAccess { see: TeamRole::Viewer, open: TeamRole::Member, people: Vec::new() }
    }
}

impl GroupAccess {
    /// The rules everyone had before groups had any: the whole team sees, members open.
    pub fn is_default(&self) -> bool {
        *self == GroupAccess::default()
    }

    pub fn may_see(&self, user_id: &str, role: TeamRole) -> bool {
        role == TeamRole::Owner || role >= self.see || self.people.iter().any(|p| p == user_id)
    }

    pub fn may_open(&self, user_id: &str, role: TeamRole) -> bool {
        if role == TeamRole::Owner {
            return true;
        }
        role >= TeamRole::Member && (role >= self.open.max(self.see) || self.people.iter().any(|p| p == user_id))
    }

    /// The role that counts for one profile of the group: `None` = it is hidden from this person; a viewer's role
    /// when they may only look.
    pub fn effective(&self, user_id: &str, role: TeamRole) -> Option<TeamRole> {
        if !self.may_see(user_id, role) {
            None
        } else if self.may_open(user_id, role) {
            Some(role)
        } else {
            Some(TeamRole::Viewer)
        }
    }

    /// In words, for lists ("Admins and owners", "Everyone").
    pub fn describe(&self) -> String {
        let who = |r: TeamRole| match r {
            TeamRole::Viewer => "everyone",
            TeamRole::Member => "members and up",
            TeamRole::Admin => "admins and owners",
            TeamRole::Owner => "owners",
        };
        let extra =
            if self.people.is_empty() { String::new() } else { format!(", plus {} chosen people", self.people.len()) };
        let verb = |r: TeamRole| if r == TeamRole::Viewer { "sees" } else { "see" };
        if self.see == self.open.max(self.see) {
            format!("Only {} see and open them{extra}", who(self.see))
        } else {
            format!("{} {} them, {} open them{extra}", capitalize(who(self.see)), verb(self.see), who(self.open))
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use TeamRole::*;

    #[test]
    fn the_default_is_what_teams_had_before() {
        let a = GroupAccess::default();
        assert_eq!(
            (a.effective("u", Viewer), a.effective("u", Member), a.effective("u", Admin)),
            (Some(Viewer), Some(Member), Some(Admin))
        );
    }

    #[test]
    fn admins_only_hides_the_group_from_members_but_never_from_owners() {
        let a = GroupAccess { see: Admin, open: Admin, people: vec!["ann".into()] };
        assert_eq!(a.effective("bob", Member), None);
        assert_eq!(a.effective("bob", Viewer), None);
        assert_eq!(a.effective("bob", Admin), Some(Admin));
        assert_eq!(a.effective("ann", Member), Some(Member), "a chosen person may use it");
        assert_eq!(a.effective("ann", Viewer), Some(Viewer), "a chosen viewer still only looks");
        let owners = GroupAccess { see: Owner, open: Owner, people: vec![] };
        assert_eq!(owners.effective("zed", Owner), Some(Owner));
        assert_eq!(owners.effective("adm", Admin), None);
    }

    #[test]
    fn members_may_look_but_only_admins_open() {
        let a = GroupAccess { see: Viewer, open: Admin, people: vec![] };
        assert_eq!(a.effective("bob", Member), Some(Viewer));
        assert_eq!(a.effective("adm", Admin), Some(Admin));
        // `open` lower than `see` cannot let someone open what they cannot see
        let odd = GroupAccess { see: Admin, open: Member, people: vec![] };
        assert_eq!(odd.effective("bob", Member), None);
        assert!(!odd.may_open("bob", Member));
    }
}
