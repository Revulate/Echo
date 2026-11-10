# 7TV EventAPI Replay Harness

Echo can replay captured 7TV EventAPI payloads without relying on a live emote edit.
This is intended for Windows-safe validation of the EventAPI parser, direct patch path,
refresh fallback path, retokenization, and layout invalidation.

The add/remove/rename fixture also carries `body.actor.display_name`. After Echo has
applied a direct patch or authoritative reconciliation, an ordinary channel receives
one timestamped chat system row per change, for example `fixture_editor added 7TV emote
EchoAdded.` Global, personal, stale, and failed refreshes intentionally emit no row.

## Quick Run

```powershell
.\scripts\run-7tv-eventapi-replay.ps1 `
  -Fixture "benchmarks\fixtures\7tv-eventapi\emote-set-add-remove-rename.json" `
  -SetId "example_set_id" `
  -Channel "examplechannel" `
  -ReplayOnly
```

The bundled fixture uses a synthetic set ID, so its quick run is replay-only.
Omit `-ReplayOnly` only when mapping a real current 7TV set ID. The script writes
stdout/stderr logs to `debug-logs\echo-7tv-replay-*.log`.

On Linux without PowerShell, use the same environment variables directly and bound
the persistent GUI process with `timeout`:

```bash
timeout 30s env \
  ECHO_LOG="irc,emotes,cache,perf,perf_cache,perf_emotes,perf_render,ui,net" \
  ECHO_7TV_EVENTAPI_REPLAY="$PWD/benchmarks/fixtures/7tv-eventapi/emote-set-add-remove-rename.json" \
  ECHO_7TV_EVENTAPI_REPLAY_SET_ID="example_set_id" \
  ECHO_7TV_EVENTAPI_REPLAY_CHANNEL="examplechannel" \
  ECHO_7TV_EVENTAPI_REPLAY_DELAY_MS=0 \
  ECHO_7TV_EVENTAPI_REPLAY_ONLY=1 \
  ./target/debug/echo
```

Exit status `124` from `timeout` is expected after the replay markers appear because
Echo remains open as a GUI app.

## Environment

```powershell
$env:ECHO_LOG = "emotes,perf,perf_emotes,perf_render,ui"
$env:ECHO_PERF_MONITOR = "1"
$env:RUST_LOG = "debug"
$env:ECHO_7TV_EVENTAPI_REPLAY = "C:\path\to\fixture.json"
$env:ECHO_7TV_EVENTAPI_REPLAY_SET_ID = "01F1DCCAW0000B9MRY8PFTNSJ9"
$env:ECHO_7TV_EVENTAPI_REPLAY_CHANNEL = "examplechannel"
$env:ECHO_7TV_EVENTAPI_REPLAY_DELAY_MS = "3000"
cargo run --bin echo
```

Set `ECHO_7TV_EVENTAPI_REPLAY_ONLY=1` to skip the live websocket and run only the
fixture replay worker. Without replay-only mode, Echo starts the normal websocket and
also replays the fixture after the delay.

Use `ECHO_7TV_EVENTAPI_REPLAY_CHANNEL=global` for a global emote set fixture.

For shared-set routing, map one fixture set to multiple owners:

```powershell
$env:ECHO_7TV_EVENTAPI_REPLAY_MAPPINGS = "01F1DCCAW0000B9MRY8PFTNSJ9:channel_one,01F1DCCAW0000B9MRY8PFTNSJ9:channel_two"
```

For `user.update` active-set fixtures, provide owner metadata so Echo registers
the same `user.update` subscription shape used by live 7TV payloads:

```powershell
$env:ECHO_7TV_EVENTAPI_REPLAY_SET_ID = "old_set_id"
$env:ECHO_7TV_EVENTAPI_REPLAY_CHANNEL = "examplechannel"
$env:ECHO_7TV_EVENTAPI_REPLAY_USER_ID = "7tv_user_id"
$env:ECHO_7TV_EVENTAPI_REPLAY_CONNECTION_INDEX = "0"
```

Example fixtures live in `benchmarks/fixtures/7tv-eventapi`.

`user-update-active-set-clear.json` covers an active-set removal by sending an empty
new set id for the same user/connection shape.

`connection-control.json` covers HELLO/session capture, server-requested reconnect,
nonfatal error code 4009, reconnectable error code 4008, explicit RESUME rejection,
fresh-session reconciliation, and slow reconnect code 4007. In replay-only mode,
HELLO/RECONNECT/ERROR/END_OF_STREAM envelopes pass through the production message
handler and log the resulting connection control. The replay does not instantiate a
WebSocket server, so it validates classification and state publication rather than
the supervisor physically opening a replacement socket.

## Fixture Formats

The replay file can be a single EventAPI message:

```json
{
  "op": 0,
  "d": {
    "type": "emote_set.update",
    "body": {
      "id": "01F1DCCAW0000B9MRY8PFTNSJ9",
      "pushed": []
    }
  }
}
```

It can also be a raw dispatch object:

```json
{
  "type": "emote_set.update",
  "body": {
    "id": "01F1DCCAW0000B9MRY8PFTNSJ9",
    "pulled": []
  }
}
```

Arrays of objects and JSONL files are supported. Direct patching requires complete
7TV emote payloads in `value`; partial or incompletely matched payloads intentionally
fall back to an authoritative fetch of the exact 7TV set. That fallback replaces only
the 7TV provider slice and does not require a cached Twitch channel ID.

## Expected Log Markers

- `7tv.patch.applied`: direct in-memory patch succeeded.
- `7tv.patch.fallback_refresh`: cache was missing or patch could not be applied.
- `7tv.patch.incomplete_cache_match`: at least one removed/updated old identity was
  not resident, so the partial result was not accepted as authoritative.
- `7tv.patch.exact_set_reconcile.started`: exact provider-scoped recovery started.
- `7tv.patch.exact_set_reconcile.applied`: the authoritative 7TV set replaced the
  channel's 7TV provider slice.
- `7tv.patch.exact_set_reconcile.preserved`: the authoritative fetch failed and no
  replacement was applied.
- `7tv.patch.partial_payload_refresh`: fixture did not contain enough emote data for direct patching.
- `7tv.refresh_batch`: coalesced refresh worker drained a replay/live batch.
- `7tv.eventapi.server_reconnect`: opcode 4 requested reconnect.
- `7tv.eventapi.reconnectable_error`: a structured 4000/4006/4007/4008 error requested reconnect.
- `7tv.eventapi.resume_session_rejected`: a rejected RESUME cleared the saved session.
- `7tv.eventapi.fresh_session_reconcile_queued`: the next HELLO queued catch-up for owned sets.
- `7tv.eventapi.end_of_stream`: opcode 7 requested reconnect; code 4007 selects the slow delay.
- `7tv.user_update.active_set`: active-set change was detected from `user.update`.
- `7tv.user_update.active_set.stale`: an older active-set fetch was correctly dropped.
- `7tv.user_update.active_set.applied`: active-set fetch/apply completed.
- `7tv.user_update.active_set.failed`: active-set fetch/apply failed.
- `7tv.user_update.active_set.cleared`: active-set removal cleared channel 7TV state.

Cold replay-only runs can legitimately emit fallback/exact-set markers instead of
`7tv.patch.applied` because channel/global/user emote caches may not be seeded before
fixture dispatch. Those runs still validate dispatch parsing, ownership routing,
incomplete-patch detection, and provider-scoped recovery; use a cache-seeded live/debug
run for a fully matched direct-patch proof.

No `cargo test` is required for this harness.

If you set `RUST_LOG` yourself, make sure it does not suppress Echo's `info` and
`debug` targets, or the replay markers may not appear in stdout.
