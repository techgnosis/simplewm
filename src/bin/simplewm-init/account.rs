//! The accounts simplewm-init needs, from `/etc/passwd` and `/etc/group`.

use std::path::PathBuf;

/// The user simplewm runs as: the account with this UID. No setting; the distro makes
/// exactly one such account.
pub const USER_UID: u32 = 1000;

#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
    pub shell: PathBuf,
    /// Supplementary groups (seat, audio, video, …), plus the primary group.
    pub groups: Vec<u32>,
}

impl Account {
    /// The account with `uid`, read from the system's files.
    pub fn load(uid: u32) -> Result<Account, String> {
        let passwd = std::fs::read_to_string("/etc/passwd")
            .map_err(|e| format!("can't read /etc/passwd: {e}"))?;
        let group = std::fs::read_to_string("/etc/group").unwrap_or_default();
        let mut account =
            find_in_passwd(&passwd, uid).ok_or(format!("no account with UID {uid}"))?;
        account.groups = groups_of(&group, &account.name, account.gid);
        Ok(account)
    }
}

/// The entry for `uid` in passwd-format `text` (no groups yet).
fn find_in_passwd(text: &str, uid: u32) -> Option<Account> {
    text.lines().find_map(|line| {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() < 7 || f[2].parse() != Ok(uid) {
            return None;
        }
        let shell = if f[6].is_empty() { "/bin/sh" } else { f[6] };
        Some(Account {
            name: f[0].to_string(),
            uid,
            gid: f[3].parse().ok()?,
            home: PathBuf::from(f[5]),
            shell: PathBuf::from(shell),
            groups: Vec::new(),
        })
    })
}

/// The primary group plus every group in group-format `text` that lists `name`.
fn groups_of(text: &str, name: &str, primary: u32) -> Vec<u32> {
    let mut groups = vec![primary];
    for line in text.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() < 4 || !f[3].split(',').any(|m| m == name) {
            continue;
        }
        if let Ok(gid) = f[2].parse()
            && !groups.contains(&gid)
        {
            groups.push(gid);
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/bash\n\
                          broken line\n\
                          messagebus:x:100:101:messagebus:/dev/null:/sbin/nologin\n\
                          user:x:1000:1000:user:/home/user:/bin/bash\n";
    const GROUP: &str = "root:x:0:root\nwheel:x:10:root,user\naudio:x:18:user\n\
                         video:x:27:root,user\nseat:x:103:user\nusers:x:100:games\n\
                         user:x:1000:\n";

    #[test]
    fn finds_the_user() {
        let a = find_in_passwd(PASSWD, 1000).unwrap();
        assert_eq!(a.name, "user");
        assert_eq!((a.uid, a.gid), (1000, 1000));
        assert_eq!(a.home, PathBuf::from("/home/user"));
        assert_eq!(a.shell, PathBuf::from("/bin/bash"));
        assert_eq!(
            find_in_passwd(PASSWD, 0).unwrap().home,
            PathBuf::from("/root")
        );
        assert_eq!(find_in_passwd(PASSWD, 1001), None);
    }

    #[test]
    fn empty_shell_means_sh() {
        let a = find_in_passwd("u:x:1000:1000::/home/u:\n", 1000).unwrap();
        assert_eq!(a.shell, PathBuf::from("/bin/sh"));
    }

    #[test]
    fn collects_groups() {
        assert_eq!(groups_of(GROUP, "user", 1000), vec![1000, 10, 18, 27, 103]);
        // A name that's only a prefix of a member doesn't count.
        assert_eq!(groups_of(GROUP, "use", 1000), vec![1000]);
    }
}
