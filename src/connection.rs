use crate::model::{HostProfile, Protocol};
use crate::ssh::{self, OperationLimits, RemoteFailure, RemoteManyResult, RemoteResult};
use crate::telnet;

pub fn probe(
    profile: &HostProfile,
    hosts: &[HostProfile],
    limits: OperationLimits,
) -> Result<RemoteResult, RemoteFailure> {
    match profile.protocol {
        Protocol::Ssh => ssh::probe(profile, hosts, limits),
        Protocol::Telnet => telnet::probe(profile, limits),
    }
}

pub fn execute(
    profile: &HostProfile,
    hosts: &[HostProfile],
    command: &str,
    limits: OperationLimits,
) -> Result<RemoteResult, RemoteFailure> {
    match profile.protocol {
        Protocol::Ssh => ssh::execute(profile, hosts, command, limits),
        Protocol::Telnet => telnet::execute(profile, command, limits),
    }
}

pub fn execute_many(
    profile: &HostProfile,
    hosts: &[HostProfile],
    commands: &[String],
    max_concurrency: usize,
    limits: OperationLimits,
) -> Result<RemoteManyResult, RemoteFailure> {
    match profile.protocol {
        Protocol::Ssh => ssh::execute_many(profile, hosts, commands, max_concurrency, limits),
        Protocol::Telnet => Err(RemoteFailure::new(
            "SSH_REQUIRED",
            "exec_many is available only for SSH hosts.",
        )),
    }
}
