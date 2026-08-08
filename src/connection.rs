use crate::model::{HostProfile, Protocol};
use crate::ssh::{self, OperationLimits, RemoteFailure, RemoteResult};
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
