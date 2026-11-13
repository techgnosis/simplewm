//! Sound output, behind the Sound applet. PipeWire's sound cards (its audio
//! sinks) are listed with `pw-dump`; the default one and its volume are set
//! with `wpctl` (WirePlumber), which also remembers them across restarts.

use std::process::Command;

use serde_json::Value;

/// A sound card output (a PipeWire node of class Audio/Sink).
#[derive(Debug, Clone, PartialEq)]
pub struct Sink {
    pub id: u32,
    /// What the card is called, e.g. "Built-in Audio Analog Stereo".
    pub description: String,
}

#[derive(Debug, Clone, Default)]
pub struct Outputs {
    pub sinks: Vec<Sink>,
    /// The sink sound goes to.
    pub default: Option<u32>,
}

/// The sound cards and which one is the default.
pub fn outputs() -> Result<Outputs, String> {
    let json = run(Command::new("pw-dump").arg("--no-colors"))?;
    parse_dump(&json)
}

fn parse_dump(json: &str) -> Result<Outputs, String> {
    let objects: Vec<Value> =
        serde_json::from_str(json).map_err(|e| format!("could not read pw-dump: {e}"))?;
    let mut sinks = Vec::new();
    let mut names = Vec::new();
    let mut default_name = None;
    for o in &objects {
        let props = &o["info"]["props"];
        if props["media.class"] == "Audio/Sink" {
            let Some(id) = o["id"].as_u64() else { continue };
            let name = props["node.name"].as_str().unwrap_or_default();
            let description = props["node.description"]
                .as_str()
                .or(props["node.nick"].as_str())
                .unwrap_or(name)
                .to_string();
            sinks.push(Sink {
                id: id as u32,
                description,
            });
            names.push(name.to_string());
        }
        if o["props"]["metadata.name"] == "default"
            && let Some(entries) = o["metadata"].as_array()
        {
            for e in entries {
                if e["key"] == "default.audio.sink" {
                    default_name = e["value"]["name"].as_str().map(str::to_string);
                }
            }
        }
    }
    let default = default_name
        .and_then(|d| names.iter().position(|n| *n == d))
        .map(|i| sinks[i].id);
    Ok(Outputs { sinks, default })
}

/// Volume of sink `id`, 1.0 being 100%.
pub fn volume(id: u32) -> Result<f64, String> {
    let out = run(Command::new("wpctl").args(["get-volume", &id.to_string()]))?;
    parse_volume(&out).ok_or_else(|| format!("unexpected wpctl output: {}", out.trim()))
}

fn parse_volume(out: &str) -> Option<f64> {
    out.trim()
        .strip_prefix("Volume:")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Set sink `id`'s volume (0.0 to 1.0).
pub fn set_volume(id: u32, volume: f64) -> Result<(), String> {
    let v = format!("{:.2}", volume.clamp(0.0, 1.0));
    run(Command::new("wpctl").args(["set-volume", &id.to_string(), &v])).map(drop)
}

/// Send sound to sink `id`.
pub fn set_default(id: u32) -> Result<(), String> {
    run(Command::new("wpctl").args(["set-default", &id.to_string()])).map(drop)
}

fn run(cmd: &mut Command) -> Result<String, String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let out = cmd
        .output()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            format!("{program} failed ({})", out.status)
        } else {
            err
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_volume() {
        assert_eq!(parse_volume("Volume: 0.65\n"), Some(0.65));
        assert_eq!(parse_volume("Volume: 1.00 [MUTED]\n"), Some(1.0));
        assert_eq!(parse_volume("nope"), None);
    }

    #[test]
    fn parses_dump() {
        let json = r#"[
            {"id": 51, "type": "PipeWire:Interface:Node", "info": {"props": {
                "media.class": "Audio/Sink", "node.name": "hdmi", "node.description": "HDMI Audio"}}},
            {"id": 52, "type": "PipeWire:Interface:Node", "info": {"props": {
                "media.class": "Audio/Sink", "node.name": "analog", "node.description": "Built-in Audio"}}},
            {"id": 60, "type": "PipeWire:Interface:Node", "info": {"props": {
                "media.class": "Audio/Source", "node.name": "mic"}}},
            {"id": 39, "type": "PipeWire:Interface:Metadata", "props": {"metadata.name": "default"},
             "metadata": [{"subject": 0, "key": "default.audio.sink", "value": {"name": "analog"}}]}
        ]"#;
        let o = parse_dump(json).unwrap();
        assert_eq!(
            o.sinks,
            [
                Sink {
                    id: 51,
                    description: "HDMI Audio".into()
                },
                Sink {
                    id: 52,
                    description: "Built-in Audio".into()
                }
            ]
        );
        assert_eq!(o.default, Some(52));
    }

    #[test]
    fn reads_outputs() {
        // Only where PipeWire is running.
        if let Ok(o) = outputs()
            && let Some(id) = o.default
        {
            volume(id).unwrap();
        }
    }
}
