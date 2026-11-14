//! Match rules: what a client asks the bus for with `AddMatch`, and the test of a message
//! against one (see "Match Rules" in the D-Bus specification).
//!
//! zbus has a match rule type, but its parser rejects `eavesdrop` (which `dbus-monitor` and
//! older libdbus clients send) and unquoted values, and its matcher can't resolve
//! well-known senders and treats `path_namespace` as a plain prefix. So the bus has its own.

use zbus::{Message, Structure, Variant as Value, message::Type};

/// The highest argument index a rule can name.
const MAX_ARG: u8 = 63;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    msg_type: Option<Type>,
    /// A unique name, `org.freedesktop.DBus`, or a well-known name that matches whoever
    /// owns it when the message is sent.
    pub sender: Option<String>,
    interface: Option<String>,
    member: Option<String>,
    path: Option<String>,
    path_namespace: Option<String>,
    destination: Option<String>,
    /// `argN`, sorted by N.
    args: Vec<(u8, String)>,
    /// `argNpath`, sorted by N.
    arg_paths: Vec<(u8, String)>,
    arg0namespace: Option<String>,
}

/// The string arguments of a message, as far as rules need them: `(type code, text)` for
/// strings and object paths, `None` for anything else.
pub type Args = Vec<Option<(char, String)>>;

impl Rule {
    /// Parse a rule such as `type='signal',interface='org.foo',arg0='x'`.
    pub fn parse(text: &str) -> Result<Rule, String> {
        let mut rule = Rule {
            msg_type: None,
            sender: None,
            interface: None,
            member: None,
            path: None,
            path_namespace: None,
            destination: None,
            args: Vec::new(),
            arg_paths: Vec::new(),
            arg0namespace: None,
        };
        let mut seen = Vec::new();
        for (key, value) in pairs(text)? {
            if seen.contains(&key) {
                return Err(format!("key '{key}' appears twice"));
            }
            seen.push(key.clone());
            let set = |slot: &mut Option<String>| *slot = Some(value.clone());
            match key.as_str() {
                "type" => {
                    rule.msg_type = Some(match value.as_str() {
                        "signal" => Type::Signal,
                        "method_call" => Type::MethodCall,
                        "method_return" => Type::MethodReturn,
                        "error" => Type::Error,
                        _ => return Err(format!("unknown message type '{value}'")),
                    })
                }
                "sender" => set(&mut rule.sender),
                "interface" => set(&mut rule.interface),
                "member" => set(&mut rule.member),
                "path" => set(&mut rule.path),
                "path_namespace" => set(&mut rule.path_namespace),
                "destination" => set(&mut rule.destination),
                "arg0namespace" => set(&mut rule.arg0namespace),
                // Only the bus could act on it, and simplewm doesn't eavesdrop for anyone.
                "eavesdrop" if value == "true" || value == "false" => {}
                key => {
                    let n = key
                        .strip_prefix("arg")
                        .ok_or(format!("unknown key '{key}'"))?;
                    let (n, path) = match n.strip_suffix("path") {
                        Some(n) => (n, true),
                        None => (n, false),
                    };
                    let n: u8 = n
                        .parse()
                        .ok()
                        .filter(|n| *n <= MAX_ARG)
                        .ok_or(format!("unknown key '{key}'"))?;
                    let list = if path {
                        &mut rule.arg_paths
                    } else {
                        &mut rule.args
                    };
                    list.push((n, value));
                }
            }
        }
        if rule.path.is_some() && rule.path_namespace.is_some() {
            return Err("path and path_namespace can't both be given".into());
        }
        if let Some(p) = rule.path.iter().chain(&rule.path_namespace).next()
            && !p.starts_with('/')
        {
            return Err(format!("'{p}' is not an object path"));
        }
        // argN and argNpath for the same N would ask for two things of one argument.
        if rule
            .args
            .iter()
            .any(|(n, _)| rule.arg_paths.iter().any(|(m, _)| n == m))
        {
            return Err("argN and argNpath name the same argument".into());
        }
        if rule.arg0namespace.is_some() && rule.args.iter().any(|(n, _)| *n == 0) {
            return Err("arg0 and arg0namespace can't both be given".into());
        }
        rule.args.sort();
        rule.arg_paths.sort();
        Ok(rule)
    }

    /// Whether the rule looks at arguments (so `args` needs the message body).
    pub fn wants_args(&self) -> bool {
        !self.args.is_empty() || !self.arg_paths.is_empty() || self.arg0namespace.is_some()
    }

    /// Test `msg`. `sender_ok` says whether the rule's `sender` matches the message's
    /// sender (the bus resolves well-known names). `args` gives the message's arguments,
    /// worked out once per message and only for rules that want them.
    pub fn matches(
        &self,
        msg: &Message,
        sender_ok: impl FnOnce(&str) -> bool,
        args: impl FnOnce() -> std::rc::Rc<Args>,
    ) -> bool {
        let hdr = msg.header();
        let eq = |want: &Option<String>, got: Option<&str>| {
            want.as_deref().is_none_or(|w| got == Some(w))
        };
        if self.msg_type.is_some_and(|t| t != msg.message_type())
            || !eq(&self.interface, hdr.interface().map(|i| i.as_str()))
            || !eq(&self.member, hdr.member().map(|m| m.as_str()))
            || !eq(&self.path, hdr.path().map(|p| p.as_str()))
            || !eq(&self.destination, hdr.destination().map(|d| d.as_str()))
        {
            return false;
        }
        if let Some(ns) = &self.path_namespace {
            match hdr.path() {
                Some(p) if in_namespace(p.as_str(), ns, '/') => {}
                _ => return false,
            }
        }
        if let Some(sender) = &self.sender
            && !sender_ok(sender)
        {
            return false;
        }
        if !self.wants_args() {
            return true;
        }
        let args = args();
        let string = |n: u8| match args.get(n as usize) {
            Some(Some(('s', s))) => Some(s.as_str()),
            _ => None,
        };
        if let Some(ns) = &self.arg0namespace {
            match string(0) {
                Some(a) if in_namespace(a, ns, '.') => {}
                _ => return false,
            }
        }
        if !self.args.iter().all(|(n, want)| string(*n) == Some(want)) {
            return false;
        }
        self.arg_paths
            .iter()
            .all(|(n, want)| match args.get(*n as usize) {
                Some(Some((_, got))) => path_arg_matches(want, got),
                _ => false,
            })
    }
}

/// `name` is `ns` itself or inside it (`ns` then `sep` then more).
fn in_namespace(name: &str, ns: &str, sep: char) -> bool {
    if sep == '/' && ns == "/" {
        return true;
    }
    match name.strip_prefix(ns) {
        Some(rest) => rest.is_empty() || rest.starts_with(sep),
        None => false,
    }
}

/// `argNpath`: equal, or one is a `/`-ended prefix of the other.
fn path_arg_matches(want: &str, got: &str) -> bool {
    want == got
        || (want.ends_with('/') && got.starts_with(want))
        || (got.ends_with('/') && want.starts_with(got))
}

/// The string and object path arguments of `msg` (see `Args`).
pub fn args_of(msg: &Message) -> Args {
    let body = msg.body();
    if body.is_empty() {
        return Vec::new();
    }
    let Ok(structure) = body.deserialize::<Structure>() else {
        return Vec::new();
    };
    structure
        .fields()
        .iter()
        .take(MAX_ARG as usize + 1)
        .map(|v| match v {
            Value::Str(s) => Some(('s', s.to_string())),
            Value::ObjectPath(p) => Some(('o', p.to_string())),
            _ => None,
        })
        .collect()
}

/// Split a rule into its `key=value` pairs. Values may be quoted with `'`; outside quotes,
/// `\'` is a quote character and `,` ends the value.
fn pairs(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            return Ok(out);
        }
        let mut key = String::new();
        loop {
            match chars.next() {
                Some('=') => break,
                Some(c) if c.is_ascii_alphanumeric() || c == '_' => key.push(c),
                Some(c) => return Err(format!("unexpected '{c}' in a key")),
                None => return Err(format!("key '{key}' has no value")),
            }
        }
        if key.is_empty() {
            return Err("empty key".into());
        }
        let mut value = String::new();
        let mut quoted = false;
        loop {
            match chars.next() {
                None if quoted => return Err("unterminated quote".into()),
                None => break,
                Some('\'') => quoted = !quoted,
                Some('\\') if !quoted && chars.peek() == Some(&'\'') => {
                    chars.next();
                    value.push('\'');
                }
                Some(',') if !quoted => break,
                Some(c) => value.push(c),
            }
        }
        out.push((key, value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn signal(path: &str, iface: &str, member: &str, body: &(&str, &str)) -> Message {
        Message::signal(path, iface, member)
            .sender(":1.7")
            .build(body)
            .unwrap()
    }

    fn test(rule: &str, msg: &Message) -> bool {
        Rule::parse(rule)
            .unwrap()
            .matches(msg, |s| s == ":1.7", || Rc::new(args_of(msg)))
    }

    #[test]
    fn parses() {
        let r = Rule::parse(" type='signal', interface=org.foo.Bar,arg2='x,y',eavesdrop='true'")
            .unwrap();
        assert_eq!(r.msg_type, Some(Type::Signal));
        assert_eq!(r.interface.as_deref(), Some("org.foo.Bar"));
        assert_eq!(r.args, vec![(2, "x,y".to_string())]);
        // \' outside quotes is a quote; inside, a backslash is just a backslash.
        let r = Rule::parse(r"arg0=\'a\',arg1='b\'").unwrap();
        assert_eq!(r.args[0].1, "'a'");
        assert_eq!(r.args[1].1, r"b\");
        assert_eq!(Rule::parse("").unwrap(), Rule::parse(" ").unwrap());
        // The same rule written two ways is one rule (RemoveMatch compares them).
        assert_eq!(
            Rule::parse("member='A',arg1='x',arg0='y'").unwrap(),
            Rule::parse("arg0=y,member=A,arg1=x").unwrap()
        );
        for bad in [
            "type='nope'",
            "colour='red'",
            "arg64='x'",
            "member='a',member='b'",
            "path='/a',path_namespace='/b'",
            "path='a'",
            "arg0='x',arg0path='/x'",
            "member='a",
            "member",
        ] {
            assert!(Rule::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn matches_headers() {
        let msg = signal(
            "/org/foo/bar",
            "org.foo",
            "Changed",
            &("org.foo.Thing", "/a/b"),
        );
        assert!(test("", &msg));
        assert!(test(
            "type='signal',interface='org.foo',member='Changed'",
            &msg
        ));
        assert!(!test("type='method_call'", &msg));
        assert!(!test("member='Other'", &msg));
        assert!(test("path='/org/foo/bar'", &msg));
        assert!(test("path_namespace='/org/foo'", &msg));
        assert!(test("path_namespace='/'", &msg));
        assert!(!test("path_namespace='/org/fo'", &msg));
        assert!(test("sender=':1.7'", &msg));
        assert!(!test("sender=':1.8'", &msg));
        assert!(!test("destination=':1.7'", &msg));
    }

    #[test]
    fn matches_args() {
        let msg = signal("/", "org.foo", "Changed", &("org.foo.Thing", "/a/b/"));
        assert!(test("arg0='org.foo.Thing'", &msg));
        assert!(!test("arg0='org.foo'", &msg));
        assert!(test("arg0namespace='org.foo'", &msg));
        assert!(test("arg0namespace='org.foo.Thing'", &msg));
        assert!(!test("arg0namespace='org.fo'", &msg));
        assert!(test("arg1path='/a/b/'", &msg));
        assert!(test("arg1path='/a/'", &msg));
        assert!(test("arg1path='/a/b/c'", &msg));
        assert!(!test("arg1path='/a/bc'", &msg));
        assert!(!test("arg2='x'", &msg));
        // A string rule doesn't match an argument that isn't a string.
        let num = Message::signal("/", "org.foo", "N")
            .build(&(5u32,))
            .unwrap();
        assert!(!test("arg0='5'", &num));
    }
}
