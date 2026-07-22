use serde_json::{Map, Value};
use std::error::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const PROTOCOL: u64 = 1;
const DEFAULT_SOCKET: &str = "/run/remagic/display.sock";
const MAX_REPLY_BYTES: u64 = 64 * 1024;
const TSV_HEADER: &str = "sequence\tsurface_sequence\tkey\tgeneration\tforeground_epoch\tintent\treason\tvisible_signature\tmarker\tsuccess";

pub async fn json_command(command: Value) -> Result<(), Box<dyn Error>> {
    let (reply, request_id) = request(command).await?;
    validate_reply(&reply, &request_id)?;
    println!("{}", serde_json::to_string_pretty(&reply)?);
    Ok(())
}

pub async fn submissions() -> Result<(), Box<dyn Error>> {
    let (reply, request_id) = request(serde_json::json!({"command": "status"})).await?;
    println!("{}", format_submissions(&reply, &request_id)?);
    Ok(())
}

pub async fn surface_signature(key: i32) -> Result<(), Box<dyn Error>> {
    let (reply, request_id) = request(serde_json::json!({"command": "status"})).await?;
    println!("{}", extract_surface_signature(&reply, &request_id, key)?);
    Ok(())
}

async fn request(mut command: Value) -> Result<(Value, String), Box<dyn Error>> {
    let socket =
        std::env::var("REMAGIC_DISPLAY_SOCKET").unwrap_or_else(|_| DEFAULT_SOCKET.to_owned());
    let request_id = format!("remagicctl-{}", std::process::id());
    command["protocol"] = PROTOCOL.into();
    command["request_id"] = request_id.clone().into();

    let mut stream = UnixStream::connect(socket).await?;
    stream.write_all(&serde_json::to_vec(&command)?).await?;
    stream.write_all(b"\n").await?;
    stream.shutdown().await?;

    let mut encoded = String::new();
    let bytes = BufReader::new(stream)
        .take(MAX_REPLY_BYTES + 1)
        .read_to_string(&mut encoded)
        .await?;
    if bytes == 0 {
        return Err("display host returned an empty reply".into());
    }
    if bytes as u64 > MAX_REPLY_BYTES
        || !encoded.ends_with('\n')
        || encoded[..encoded.len() - 1].contains('\n')
    {
        return Err("display host reply exceeded the bounded single-line protocol".into());
    }
    let reply = serde_json::from_str(&encoded)?;
    Ok((reply, request_id))
}

fn validate_reply<'a>(
    reply: &'a Value,
    expected_request_id: &str,
) -> Result<&'a Map<String, Value>, String> {
    let object = reply
        .as_object()
        .ok_or_else(|| "display host reply is not an object".to_owned())?;
    if required_u64(object, "protocol")? != PROTOCOL {
        return Err("display host reply used an unsupported protocol".into());
    }
    if required_str(object, "request_id")? != expected_request_id {
        return Err("display host reply request_id does not match the request".into());
    }
    let ok = object
        .get("ok")
        .and_then(Value::as_bool)
        .ok_or_else(|| "display host reply omitted boolean ok".to_owned())?;
    required_str(object, "status")?;
    if !ok {
        let error = object
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("display host rejected command");
        return Err(error.to_owned());
    }
    Ok(object)
}

fn format_submissions(reply: &Value, expected_request_id: &str) -> Result<String, String> {
    let object = validate_reply(reply, expected_request_id)?;
    let snapshot = object
        .get("snapshot")
        .and_then(Value::as_object)
        .ok_or_else(|| "display status reply omitted its snapshot".to_owned())?;
    let submissions = snapshot
        .get("recent_submissions")
        .and_then(Value::as_array)
        .ok_or_else(|| "display status snapshot omitted recent_submissions".to_owned())?;

    let mut output = String::from(TSV_HEADER);
    let mut previous_sequence = 0;
    for (index, submission) in submissions.iter().enumerate() {
        let record = submission
            .as_object()
            .ok_or_else(|| format!("recent_submissions[{index}] is not an object"))?;
        let sequence = required_u64(record, "sequence")?;
        if sequence == 0 || sequence <= previous_sequence {
            return Err(format!(
                "recent_submissions[{index}] has a zero or non-increasing sequence"
            ));
        }
        previous_sequence = sequence;
        let surface_sequence = required_u64(record, "surface_sequence")?;
        let key = required_i32(record, "key")?;
        let generation = required_u64(record, "generation")?;
        let foreground_epoch = required_u64(record, "foreground_epoch")?;
        let intent = required_enum(
            record,
            "intent",
            &["ink", "mono_quality", "ui", "content", "full"],
        )?;
        let reason = required_enum(
            record,
            "reason",
            &[
                "foreground_switch",
                "surface_damage",
                "full_refresh",
                "lock_screen",
                "lock_refresh",
                "unlock_screen",
                "live_ink",
                "canonical_settle",
            ],
        )?;
        let visible_signature = required_u64(record, "visible_signature")?;
        let marker = optional_u64(record, "marker")?.unwrap_or(0);
        let success = record
            .get("success")
            .and_then(Value::as_bool)
            .ok_or_else(|| format!("recent_submissions[{index}].success is not a boolean"))?;
        if success && marker == 0 {
            return Err(format!(
                "recent_submissions[{index}] marks a zero-marker submission successful"
            ));
        }
        if !success && marker != 0 {
            return Err(format!(
                "recent_submissions[{index}] attaches a marker to a failed submission"
            ));
        }
        output.push('\n');
        output.push_str(&format!(
            "{sequence}\t{surface_sequence}\t{key}\t{generation}\t{foreground_epoch}\t{intent}\t{reason}\t{visible_signature}\t{marker}\t{success}"
        ));
    }
    Ok(output)
}

fn extract_surface_signature(
    reply: &Value,
    expected_request_id: &str,
    key: i32,
) -> Result<u64, String> {
    let object = validate_reply(reply, expected_request_id)?;
    let snapshot = object
        .get("snapshot")
        .and_then(Value::as_object)
        .ok_or_else(|| "display status reply omitted its snapshot".to_owned())?;
    snapshot
        .get("surface_signatures")
        .and_then(Value::as_object)
        .and_then(|signatures| signatures.get(&key.to_string()))
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("display status has no unsigned signature for surface {key}"))
}

fn required_u64(object: &Map<String, Value>, field: &str) -> Result<u64, String> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{field} is missing or is not an unsigned integer"))
}

fn optional_u64(object: &Map<String, Value>, field: &str) -> Result<Option<u64>, String> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("{field} is not an unsigned integer or null")),
    }
}

fn required_i32(object: &Map<String, Value>, field: &str) -> Result<i32, String> {
    let value = object
        .get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("{field} is missing or is not a signed integer"))?;
    i32::try_from(value).map_err(|_| format!("{field} is outside the i32 range"))
}

fn required_str<'a>(object: &'a Map<String, Value>, field: &str) -> Result<&'a str, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{field} is missing or is not a string"))
}

fn required_enum<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    allowed: &[&str],
) -> Result<&'a str, String> {
    let value = required_str(object, field)?;
    if allowed.contains(&value) {
        Ok(value)
    } else {
        Err(format!("{field} contains unsupported value {value:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(submissions: Value) -> Value {
        serde_json::json!({
            "protocol": 1,
            "request_id": "test-request",
            "ok": true,
            "status": "ok",
            "snapshot": {"recent_submissions": submissions}
        })
    }

    fn record(sequence: u64, marker: Value, success: bool) -> Value {
        serde_json::json!({
            "sequence": sequence,
            "surface_sequence": 12 + sequence,
            "key": 245209900,
            "generation": 7,
            "foreground_epoch": 19,
            "intent": "ui",
            "reason": "surface_damage",
            "visible_signature": 900 + sequence,
            "marker": marker,
            "success": success
        })
    }

    #[test]
    fn submissions_have_stable_tsv_columns_and_zero_failure_marker() {
        let formatted = format_submissions(
            &reply(serde_json::json!([
                record(41, serde_json::json!(71), true),
                record(42, Value::Null, false)
            ])),
            "test-request",
        )
        .unwrap();
        assert_eq!(
            formatted,
            concat!(
                "sequence\tsurface_sequence\tkey\tgeneration\tforeground_epoch\tintent\treason\tvisible_signature\tmarker\tsuccess\n",
                "41\t53\t245209900\t7\t19\tui\tsurface_damage\t941\t71\ttrue\n",
                "42\t54\t245209900\t7\t19\tui\tsurface_damage\t942\t0\tfalse"
            )
        );
    }

    #[test]
    fn empty_submission_history_still_has_a_stable_header() {
        assert_eq!(
            format_submissions(&reply(serde_json::json!([])), "test-request").unwrap(),
            TSV_HEADER
        );
    }

    #[test]
    fn quality_monochrome_submission_is_preserved_in_tsv() {
        let mut quality = record(1, serde_json::json!(7), true);
        quality["intent"] = serde_json::json!("mono_quality");
        let formatted =
            format_submissions(&reply(serde_json::json!([quality])), "test-request").unwrap();
        assert!(formatted.contains("\tmono_quality\tsurface_damage\t"));
    }

    #[test]
    fn surface_signature_uses_the_signature_map_not_the_sequence_map() {
        let reply = serde_json::json!({
            "protocol": 1,
            "request_id": "test-request",
            "ok": true,
            "status": "ok",
            "snapshot": {
                "surface_sequences": {"1599673631": 1},
                "surface_signatures": {"1599673631": 18446744073709551614_u64}
            }
        });
        assert_eq!(
            extract_surface_signature(&reply, "test-request", 1_599_673_631).unwrap(),
            18_446_744_073_709_551_614
        );
        assert!(extract_surface_signature(&reply, "test-request", 42).is_err());
    }

    #[test]
    fn malformed_replies_fail_closed() {
        let mut cases = vec![
            (reply(serde_json::json!([])), "wrong-request"),
            (serde_json::json!({"protocol": 1}), "test-request"),
            (
                serde_json::json!({
                    "protocol": 1,
                    "request_id": "test-request",
                    "ok": true,
                    "status": "ok"
                }),
                "test-request",
            ),
            (reply(serde_json::json!([{"sequence": 1}])), "test-request"),
            (
                reply(serde_json::json!([
                    record(2, serde_json::json!(2), true),
                    record(1, serde_json::json!(3), true)
                ])),
                "test-request",
            ),
            (
                reply(serde_json::json!([record(1, serde_json::json!(0), true)])),
                "test-request",
            ),
        ];
        for (case, request_id) in cases.drain(..) {
            assert!(
                format_submissions(&case, request_id).is_err(),
                "accepted malformed reply: {case}"
            );
        }
    }

    #[test]
    fn explicit_display_rejection_is_an_error() {
        let rejected = serde_json::json!({
            "protocol": 1,
            "request_id": "test-request",
            "ok": false,
            "status": "error",
            "error": "panel unavailable"
        });
        assert_eq!(
            format_submissions(&rejected, "test-request").unwrap_err(),
            "panel unavailable"
        );
    }
}
