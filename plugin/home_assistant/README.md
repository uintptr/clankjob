# Home Assistant plugin

Lets cases see and run your home through [Home Assistant](https://www.home-assistant.io):
read sensors and history ("did the sump pump run last night?"), run services ("turn the
porch light on at dusk"), and sleep until something happens at home ("wake me when the
parcel box opens, then email the seller"). It drives
[hacli](https://github.com/uintptr/hacli), a command-line client for Home Assistant's
REST API.

| Tool                 | What it does                                                                 | Approval                       |
| -------------------- | ---------------------------------------------------------------------------- | ------------------------------ |
| `ha_entities`        | Entity ids, states and names, one per line, by domain and/or search text     | no                             |
| `ha_state`           | One entity's state and attributes                                            | no                             |
| `ha_template`        | A Jinja2 template rendered against live state: one call for many entities    | no                             |
| `ha_history`         | An entity's state changes (default: the last 24 hours)                       | no                             |
| `ha_logbook`         | The logbook: what turned on, opened, arrived, which automation ran           | no                             |
| `ha_services`        | The services a domain offers, with their target and fields                   | no                             |
| `ha_calendar_events` | Events of a Home Assistant calendar (default: the next 14 days)              | no                             |
| `ha_call_service`    | Runs a service (light, thermostat, lock, notify, script…) with JSON data     | yes, unless in `HA_NO_APPROVAL` |

| Wait condition     | Fires when                                                                                 |
| ------------------ | ------------------------------------------------------------------------------------------ |
| `ha_state_is`      | An entity has a state (or any other, with `not`), or a number above/below a value           |
| `ha_state_changed` | An entity's state differs from when the wait started, or changed and changed back           |
| `ha_template_true` | A template renders true: several entities, attributes, time                                 |

Conditions are checked every 5 minutes by default (every minute at most). `ha_state_is`
and `ha_state_changed` read the entity's history since the previous check, so a door
opened and closed between two checks still wakes the case. `ha_template_true` only sees
the moments of its checks.

## Setup

1. Install hacli (the Docker image has it):
   `cargo install --git https://github.com/uintptr/hacli hacli`, or a
   [release binary](https://github.com/uintptr/hacli/releases).
2. In Home Assistant, create a long-lived access token: your profile, **Security** tab,
   at the bottom. The agent can do whatever this user can, so consider a dedicated user
   that is not an administrator.
3. Copy `config.example.toml` to `config.toml`, set `HA_URL` and the token reference, and
   list in `HA_NO_APPROVAL` what may run without asking you (e.g. `light, scene`).
4. Check it:

```sh
export HA_TOKEN=...                           # if config.toml uses { env = "HA_TOKEN" }
./check_config.py                             # read-only: no service is called
./check_config.py --entity lock.front_door    # also reads this entity
```

```
  ok   hacli on PATH (/usr/local/bin/hacli)
  ok   Home Assistant at http://homeassistant.local:8123 accepts the token
  ok   Home Assistant 2026.9.3, home 'Home', time zone America/Toronto
  ok   870 entities
  ok   templates render
  ok   278 services
  ok   HA_NO_APPROVAL names known domains and services (light, scene)
  ok   lock.front_door is locked
```

Tests use a fake hacli. `HA_LIVE_TEST=1` also runs the read-only checks against
`HA_URL` with `HA_TOKEN`:

```sh
python3 -m unittest -v test_ha_tool.py
```

## Design

`ha_tool.py` is a command plugin ([design §9.9](../../docs/design.md)), standard library
only, one subcommand per tool and condition. It runs `hacli -o json …` with `HA_URL` and
`HA_TOKEN` in its environment (never on its command line) and compacts what hacli prints.

- **Settings.** `HA_URL` and `HA_TOKEN` are both required, so hacli never falls back to a
  `~/.config/hacli/config.toml` that happens to be on the machine. The token is replaced
  by `***` in any error text. hacli's own error line (`Error: …`) becomes the tool
  error, with a hint for a refused token, an unreachable server or an unknown entity.
- **Compact output.** A home has hundreds of entities and services, and hacli prints Home
  Assistant's full JSON. `ha_entities` prints one line per entity
  (`sensor.kitchen = 21.5 °C  (Kitchen)`), `ha_services` one line per service with its
  target and field names (`*` = required; collapsed sections flattened), and
  `ha_history` a list of `{state, when}` (attributes only on request). Long results are
  stored as case files (`output = "auto"`).
- **Times.** `start` and `end` take a date or an ISO 8601 time; one without an offset is
  in the server's local time (`TZ`).
- **Service calls.** The agent passes `data` as one JSON object; each key becomes a
  hacli `--field key=<value as JSON>`, which hacli reads back as JSON, so types survive
  (`"on"` stays a string, `200` a number, a list of entity ids a list). With
  `return_response`, the service's answer is returned (needed by services such as
  `weather.get_forecasts`); otherwise only the entities it changed.
- **Approval** ([design §9.7](../../docs/design.md)). `ha_call_service` always asks the
  owner, except for the domains and services in `HA_NO_APPROVAL` (its `approval_check`,
  `ha_tool.py needs-approval`, answers `required: false` for them). The case's
  **Approvals** setting still applies: `always` asks for those too, `never` asks for none.
- **Denied services.** `HA_DENIED_SERVICES` is never called, approved or not. The
  default leaves out `homeassistant.stop` and `.restart`, `hassio` (add-ons), `backup`
  and `recorder` (purges); `""` allows everything. `lock`, `alarm_control_panel` or
  `cover` are worth adding if the agent should never touch them.
- **Conditions.** They print `{status, events, cursor}` (§9.9). `ha_state_is` fires at
  once when the state is already the one wanted; its cursor is the time of the last
  check, and each later check also reads `history` since then. `ha_state_changed` stores
  the state it started from, then fires on any change in the history or in the current
  state. A number test ignores `unavailable` and `unknown`.
- **Repeats.** Reading is safe to repeat. A service call runs again if the server stops
  mid-call (§9.9); most services (turn on, set temperature) do the same thing twice.
- `requires_config = true` and `requires = ["python3", "hacli"]`: the plugin stays
  unloaded without its config or without hacli.

Planned: waking on Home Assistant events as they happen (the WebSocket API, which a
command plugin cannot hold open), areas and devices (only in the WebSocket API), and a
guide for writing templates.
