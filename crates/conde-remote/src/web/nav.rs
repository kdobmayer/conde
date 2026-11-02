//! The masthead's lists on the signed-in server pages: web/src/lib/nav.svelte.ts over the same
//! `owner.list` and `fleet.list` answers the app reads, so a server page's masthead shows what
//! the app's does.

use serde_json::Value;

/// One of the owner's agents (not a human) with its state.
pub(crate) struct NavAgent {
    /// The short name: unqualified in the owner's namespace.
    pub name: String,
    /// `online`, `offline`, or `unbound` (on no host, or the fleet did not answer).
    pub state: &'static str,
    /// Held and stuck deliveries, when there are any.
    pub held: Option<u64>,
    pub stuck: Option<u64>,
}

/// A channel one of the owner's agents is in.
pub(crate) struct NavChannel {
    /// With its `#`.
    pub name: String,
    /// Without it, for the app's route.
    pub slug: String,
}

pub(crate) struct Nav {
    /// `None` when `owner.list` did not answer.
    pub agents: Option<Vec<NavAgent>>,
    /// Every channel any of the owner's agents is in, once, sorted.
    pub channels: Vec<NavChannel>,
    /// The user is a workspace admin (`fleet.list`'s `admins`).
    pub admin: bool,
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn list<'a>(v: Option<&'a Value>, key: &str) -> &'a [Value] {
    v.and_then(|v| v.get(key))
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

impl Nav {
    /// `rail` is the `owner.list` result and `fleet` the `fleet.list` result of `user`.
    pub(crate) fn new(user: &str, rail: Option<&Value>, fleet: Option<&Value>) -> Nav {
        let owner = rail.map_or("", |r| str_of(r, "owner"));
        let hosts = list(fleet, "hosts");
        let bound = list(fleet, "agents");
        let short = |name: &str| {
            name.strip_prefix(user)
                .and_then(|rest| rest.strip_prefix('/'))
                .unwrap_or(name)
                .to_string()
        };
        let agents = rail.map(|r| {
            list(Some(r), "agents")
                .iter()
                .filter(|a| str_of(a, "kind") != "human")
                .map(|a| {
                    let name = str_of(a, "name");
                    let on = bound
                        .iter()
                        .find(|f| str_of(f, "owner") == owner && str_of(f, "name") == name);
                    let online = on.is_some_and(|f| {
                        hosts.iter().any(|h| {
                            str_of(h, "owner") == owner
                                && str_of(h, "host") == str_of(f, "host")
                                && h.get("online").and_then(Value::as_bool) == Some(true)
                        })
                    });
                    let count = |key| {
                        on.and_then(|f| f.get(key))
                            .and_then(Value::as_u64)
                            .filter(|&n| n > 0)
                    };
                    NavAgent {
                        name: short(name),
                        state: match (on, online) {
                            (None, _) => "unbound",
                            (Some(_), true) => "online",
                            (Some(_), false) => "offline",
                        },
                        held: count("held"),
                        stuck: count("stuck"),
                    }
                })
                .collect()
        });
        let mut names: Vec<&str> = list(rail, "agents")
            .iter()
            .flat_map(|a| list(Some(a), "channels"))
            .map(|c| str_of(c, "channel"))
            .filter(|c| !c.is_empty())
            .collect();
        names.sort_unstable();
        names.dedup();
        let channels = names
            .into_iter()
            .map(|c| NavChannel {
                name: c.to_string(),
                slug: c.strip_prefix('#').unwrap_or(c).to_string(),
            })
            .collect();
        let admin = list(fleet, "admins")
            .iter()
            .any(|a| a.as_str() == Some(user));
        Nav {
            agents,
            channels,
            admin,
        }
    }

    /// The masthead's scope note, as the app words it.
    pub(crate) fn scope(&self) -> String {
        let n = self.agents.as_ref().map_or(0, Vec::len);
        let agents = match n {
            1 => "agent".to_string(),
            0 => "agents".to_string(),
            n => format!("{n} agents"),
        };
        format!("You see what your {agents} can read.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agents_take_their_state_from_the_fleet_and_channels_are_listed_once() {
        let rail = json!({"owner": "ann", "agents": [
            {"name": "ann/desk", "kind": "claude", "channels": [{"channel": "#ops"}, {"channel": "#dev"}]},
            {"name": "ann/astra", "kind": "codex", "channels": [{"channel": "#ops"}]},
            {"name": "ann/new", "kind": null, "channels": []},
            {"name": "ann/ann", "kind": "human", "channels": [{"channel": "#people"}]},
        ]});
        let fleet = json!({"admins": ["ann"], "hosts": [
            {"owner": "ann", "host": "pc", "online": true},
            {"owner": "ann", "host": "box", "online": false},
            {"owner": "bob", "host": "box", "online": true},
        ], "agents": [
            {"owner": "ann", "name": "ann/desk", "host": "pc", "held": 2, "stuck": 0},
            {"owner": "ann", "name": "ann/astra", "host": "box", "held": 0, "stuck": 1},
        ]});
        let nav = Nav::new("ann", Some(&rail), Some(&fleet));
        let agents: Vec<_> = nav
            .agents
            .as_ref()
            .unwrap()
            .iter()
            .map(|a| (a.name.as_str(), a.state, a.held, a.stuck))
            .collect();
        assert_eq!(
            agents,
            [
                ("desk", "online", Some(2), None),
                ("astra", "offline", None, Some(1)),
                ("new", "unbound", None, None),
            ]
        );
        let channels: Vec<_> = nav
            .channels
            .iter()
            .map(|c| (c.name.as_str(), c.slug.as_str()))
            .collect();
        assert_eq!(
            channels,
            [("#dev", "dev"), ("#ops", "ops"), ("#people", "people")]
        );
        assert!(nav.admin);
        assert_eq!(nav.scope(), "You see what your 3 agents can read.");
    }

    #[test]
    fn without_the_fleet_every_agent_is_unbound_and_without_the_rail_none_is_listed() {
        let rail = json!({"owner": "ann", "agents": [{"name": "ann/desk", "kind": "claude", "channels": []}]});
        let nav = Nav::new("ann", Some(&rail), None);
        let agents = nav.agents.as_ref().unwrap();
        assert_eq!(
            (agents[0].name.as_str(), agents[0].state),
            ("desk", "unbound")
        );
        assert!(!nav.admin);
        assert_eq!(nav.scope(), "You see what your agent can read.");
        let none = Nav::new("ann", None, None);
        assert!(none.agents.is_none() && none.channels.is_empty());
        assert_eq!(none.scope(), "You see what your agents can read.");
    }
}
