//! The home partition. On the user's machine home folders live on their own partition,
//! named `distro-home` (its GPT name), so installing a new system over the root
//! partition leaves them alone. simplewm-init mounts it on /home. Without one (the VM)
//! /home is simply the root filesystem's.

use std::path::PathBuf;

use crate::sys;

/// The GPT partition name `distro/install.sh` gives the home partition.
pub const PARTITION_NAME: &str = "distro-home";
pub const MOUNT_POINT: &str = "/home";

/// What happened to /home, for the console.
pub enum Outcome {
    /// No partition of that name: /home is the root filesystem's.
    NoPartition,
    Mounted(PathBuf),
    Failed(PathBuf, std::io::Error),
}

/// Mount the home partition on /home, if there is one.
pub fn mount() -> Outcome {
    let Some(dev) = find_partition(PARTITION_NAME) else {
        return Outcome::NoPartition;
    };
    let flags = libc::MS_NOSUID | libc::MS_NODEV;
    match sys::mount(&dev.to_string_lossy(), MOUNT_POINT, "ext4", flags, "") {
        Ok(()) => Outcome::Mounted(dev),
        Err(e) => Outcome::Failed(dev, e),
    }
}

/// The device file of the partition with this GPT name, from the kernel's description of
/// each block device (`/sys/class/block/*/uevent`).
fn find_partition(name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir("/sys/class/block").ok()?;
    entries.flatten().find_map(|entry| {
        let uevent = std::fs::read_to_string(entry.path().join("uevent")).ok()?;
        partition_device(&uevent, name)
    })
}

/// `/dev/<DEVNAME>` if this uevent describes a partition called `name`.
fn partition_device(uevent: &str, name: &str) -> Option<PathBuf> {
    let field = |key: &str| {
        uevent
            .lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
    };
    if field("DEVTYPE")? != "partition" || field("PARTNAME")? != name {
        return None;
    }
    Some(PathBuf::from("/dev").join(field("DEVNAME")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "MAJOR=8\nMINOR=19\nDEVNAME=sdb3\nDEVTYPE=partition\n\
                        DISKSEQ=2\nPARTN=3\nPARTNAME=distro-home\n";

    #[test]
    fn finds_the_named_partition() {
        assert_eq!(
            partition_device(HOME, "distro-home"),
            Some(PathBuf::from("/dev/sdb3"))
        );
    }

    #[test]
    fn skips_other_partitions_and_disks() {
        let root = HOME.replace("distro-home", "distro-root");
        assert_eq!(partition_device(&root, "distro-home"), None);
        // A partition name that only starts with ours.
        let longer = HOME.replace("distro-home", "distro-home2");
        assert_eq!(partition_device(&longer, "distro-home"), None);
        let disk = "MAJOR=8\nMINOR=16\nDEVNAME=sdb\nDEVTYPE=disk\nDISKSEQ=2\n";
        assert_eq!(partition_device(disk, "distro-home"), None);
        // MBR partitions have no name.
        let mbr = "MAJOR=8\nMINOR=1\nDEVNAME=sda1\nDEVTYPE=partition\nPARTN=1\n";
        assert_eq!(partition_device(mbr, "distro-home"), None);
    }
}
