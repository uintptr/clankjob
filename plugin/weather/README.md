# Weather plugin (Open-Meteo)

Gives cases the weather: the forecast for planning a visit or outdoor work, and what the
weather was on a past day, for checking a claim ("the storm on July 15 broke the fence",
"the pipe froze last week"). Data comes from [Open-Meteo](https://open-meteo.com), free for
non-commercial use and without an API key.

| Tool               | What it does                                                                                    |
| ------------------ | ----------------------------------------------------------------------------------------------- |
| `weather_forecast` | Current conditions and up to 16 days ahead (hour by hour for up to 3): temperature, rain, wind… |
| `weather_history`  | Any past days since 1940, up to yesterday (hour by hour for up to 7): rain, snow, wind, gusts…  |

A location is a place name, best narrowed by region or country in full (`Laval, Quebec`,
`Paris, France`), or coordinates (`45.51,-73.59`). The result always says which place it
resolved to, lists other places of the same name, and adds a `location_note` when the
region given matched none of them (abbreviations like `QC` or `TX` do not): the first
place found is used, and the agent can ask again more precisely.

## Setup

Nothing to configure. To use a commercial Open-Meteo key, copy `config.example.toml` to
`config.toml` and uncomment `OPEN_METEO_API_KEY`. Then check it:

```sh
export OPEN_METEO_API_KEY=...    # only if config.toml uses { env = "OPEN_METEO_API_KEY" }
./check_config.py                # or --location "Paris, France"
```

```
  ok   Open-Meteo free servers
  ok   'Montreal, Quebec' is Montreal, Quebec, Canada (45.50884, -73.58781)
  ok   forecast: 14.0 °C now, 2 days ahead
  ok   past weather on 2026-09-22
```

Tests use a fake Open-Meteo; `WEATHER_LIVE_TEST=1` also runs the checks above:

```sh
python3 -m unittest -v test_weather_tool.py
```

## Design

`weather_tool.py` is a command plugin ([design §9.9](../../docs/design.md)), standard
library only, one subcommand per tool, printing JSON.

- **Places.** Names go through Open-Meteo's geocoding API. The text before the first
  comma is searched; each part after it must match (exactly or as a prefix, ignoring
  case) the region, subregion, country or country code of a result. Coordinates skip the
  lookup. The location is passed as `--location={location}` so that a negative latitude
  is not refused as an option by the host.
- **Forecast** (`api.open-meteo.com/v1/forecast`): `current`, `daily` and optionally
  `hourly` variables, `timezone=auto` so times are the place's local time.
- **History** (`archive-api.open-meteo.com/v1/archive`): ERA5-based reanalysis on a 9 to
  25 km grid, not station readings; the tool description warns the agent that local
  extremes (a single gust, hail) may be smoothed out. The archive lags a day or more:
  `end` is capped at yesterday (UTC), and days still without data are dropped and named
  in a `note`. Long ranges (up to 366 days) are stored as a case file (`output = "auto"`).
- **Output.** Open-Meteo's columns become one row per day or hour; WMO weather codes
  become words (`moderate rain`); units are listed once in `units`. `units = imperial`
  asks for °F, mph and inches.
- **Key.** With `OPEN_METEO_API_KEY` set, requests go to the `customer-` servers with
  `apikey`; the key is replaced by `***` in any error text.
- Every call is read-only and safe to repeat; each HTTP request times out after 30 s.

Planned: a wait condition that fires when the forecast crosses a threshold (e.g. no rain
for the next two days, or frost overnight), and weather alerts.
