//! Board operations, one function per op, all run inside the dispatcher's
//! transaction.

pub mod agents;
pub mod bridge;
pub mod channels;
pub mod hosts;
pub mod messages;
pub mod misc;
pub mod web;

use crate::dispatch::OpSpec;

macro_rules! op {
    ($name:literal, $run:path, mutation) => {
        OpSpec { name: $name, mutation: true, bridge_only: false, run: $run }
    };
    ($name:literal, $run:path, query) => {
        OpSpec { name: $name, mutation: false, bridge_only: false, run: $run }
    };
    ($name:literal, $run:path, bridge) => {
        OpSpec { name: $name, mutation: true, bridge_only: true, run: $run }
    };
}

pub static OPS: &[OpSpec] = &[
    op!("agent.register", agents::register, mutation),
    op!("agent.unregister", agents::unregister, mutation),
    op!("agent.pin", agents::pin, mutation),
    op!("agent.whoami", agents::whoami, query),
    op!("agent.resolve", agents::resolve, query),
    op!("agent.list", agents::list, query),
    op!("profile.get", agents::profile_get, query),
    op!("profile.set", agents::profile_set, mutation),
    op!("message.send", messages::send, mutation),
    op!("message.read", messages::read, mutation),
    op!("message.get", messages::get, query),
    op!("attachment.get", messages::attachment_get, query),
    op!("inbox.list", messages::inbox, query),
    op!("reaction.set", messages::reaction_set, mutation),
    op!("search", messages::search, query),
    op!("receipt.list", messages::receipts, query),
    op!("channel.create", channels::create, mutation),
    op!("channel.join", channels::join, mutation),
    op!("channel.leave", channels::leave, mutation),
    op!("channel.topic", channels::topic, mutation),
    op!("channel.invite", channels::invite, mutation),
    op!("channel.list", channels::list, query),
    op!("channel.members", channels::members, query),
    op!("conversation.list", web::list, query),
    op!("conversation.history", web::history, query),
    op!("conversation.read", web::read, mutation),
    op!("fleet.list", web::fleet, query),
    op!("doctor", misc::doctor, query),
    op!("bridge.claim", bridge::claim, bridge),
    op!("bridge.report", bridge::report, bridge),
    op!("bridge.fence", bridge::fence, bridge),
    op!("bridge.endpoints", bridge::endpoints, bridge),
];
