//! Reads a client routes to a daemon that already holds the vault's state
//! (Roadmap 10.5, `docs/specs/index-ownership-and-client-routing.md`).
//!
//! Each routed operation has one implementation here, run by the CLI against
//! the cache directly and by the daemon against its retained session, so a
//! routed answer is the direct answer computed elsewhere. Results cross the
//! wire as full-fidelity [`wire`] types and are printed by the client.

pub mod dql;
pub mod ls;
pub mod query;
pub mod search;
pub mod tasks;
pub mod wire;

#[cfg(test)]
pub(crate) mod test_support {
    /// Copy the fixture vault `name` into `destination`.
    pub(crate) fn copy_fixture(name: &str, destination: &std::path::Path) {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);
        let mut pending = vec![source.clone()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                let target = destination.join(path.strip_prefix(&source).unwrap());
                if path.is_dir() {
                    std::fs::create_dir_all(&target).unwrap();
                    pending.push(path);
                } else {
                    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                    std::fs::copy(&path, &target).unwrap();
                }
            }
        }
    }
}
