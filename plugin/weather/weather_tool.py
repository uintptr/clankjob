#!/usr/bin/env python3
"""clankjob weather plugin: forecasts and past weather for a place, from Open-Meteo.

A command plugin (design section 9.9): each tool call runs one subcommand and prints JSON.
Standard library only. Open-Meteo needs no key for non-commercial use; set
OPEN_METEO_API_KEY (config.toml [env]) to use its commercial servers instead.

    forecast --location=L [--days N] [--hourly] [--units metric|imperial]
                               current conditions and N days ahead (1 to 16)
    history --location=L --start=YYYY-MM-DD [--end=YYYY-MM-DD] [--hourly] [--units …]
                               what the weather was, from 1940 to yesterday

A location is a place name, optionally narrowed by region or country ("Laval, Quebec",
"Paris, FR"), or coordinates ("45.51,-73.59").
"""

import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from datetime import date, datetime, timedelta, timezone

Json = dict[str, object]

TIMEOUT = 30
USER_AGENT = "clankjob-weather/0.1"
MAX_FORECAST_DAYS = 16
MAX_HOURLY_FORECAST_DAYS = 3
MAX_HISTORY_DAYS = 366
MAX_HOURLY_HISTORY_DAYS = 7
FIRST_HISTORY_DAY = date(1940, 1, 1)
COORDINATES = re.compile(r"^\s*(-?\d{1,2}(?:\.\d+)?)\s*[,; ]\s*(-?\d{1,3}(?:\.\d+)?)\s*$")

UNITS = {
    "metric": {},
    "imperial": {"temperature_unit": "fahrenheit", "wind_speed_unit": "mph", "precipitation_unit": "inch"},
}
CURRENT = ("temperature_2m", "apparent_temperature", "relative_humidity_2m", "precipitation", "weather_code",
           "cloud_cover", "wind_speed_10m", "wind_gusts_10m", "wind_direction_10m")
FORECAST_DAILY = ("weather_code", "temperature_2m_max", "temperature_2m_min", "precipitation_sum",
                  "precipitation_probability_max", "snowfall_sum", "wind_speed_10m_max", "wind_gusts_10m_max",
                  "uv_index_max", "sunrise", "sunset")
FORECAST_HOURLY = ("temperature_2m", "apparent_temperature", "precipitation_probability", "precipitation",
                   "weather_code", "cloud_cover", "wind_speed_10m", "wind_gusts_10m")
HISTORY_DAILY = ("weather_code", "temperature_2m_max", "temperature_2m_min", "temperature_2m_mean",
                 "precipitation_sum", "rain_sum", "snowfall_sum", "precipitation_hours", "wind_speed_10m_max",
                 "wind_gusts_10m_max", "wind_direction_10m_dominant")
HISTORY_HOURLY = ("temperature_2m", "relative_humidity_2m", "precipitation", "rain", "snowfall", "weather_code",
                  "cloud_cover", "wind_speed_10m", "wind_gusts_10m", "wind_direction_10m")

# WMO weather interpretation codes, as used by Open-Meteo.
WEATHER_CODES = {
    0: "clear sky", 1: "mainly clear", 2: "partly cloudy", 3: "overcast", 45: "fog", 48: "depositing rime fog",
    51: "light drizzle", 53: "moderate drizzle", 55: "dense drizzle", 56: "light freezing drizzle",
    57: "dense freezing drizzle", 61: "slight rain", 63: "moderate rain", 65: "heavy rain",
    66: "light freezing rain", 67: "heavy freezing rain", 71: "slight snowfall", 73: "moderate snowfall",
    75: "heavy snowfall", 77: "snow grains", 80: "slight rain showers", 81: "moderate rain showers",
    82: "violent rain showers", 85: "slight snow showers", 86: "heavy snow showers", 95: "thunderstorm",
    96: "thunderstorm with slight hail", 99: "thunderstorm with heavy hail",
}


class ToolError(Exception):
    """A failure reported to the LLM (printed to stderr, exit status 1)."""


@dataclass(frozen=True)
class Api:
    """Open-Meteo endpoints: the free ones, or the commercial ones with an API key."""

    key: str = ""

    @classmethod
    def from_env(cls, env: dict[str, str]) -> "Api":
        return cls(env.get("OPEN_METEO_API_KEY", "").strip())

    def url(self, service: str, path: str, params: dict[str, str]) -> str:
        host = f"{'customer-' if self.key else ''}{service}.open-meteo.com"
        query = {**params, **({"apikey": self.key} if self.key else {})}
        return f"https://{host}{path}?{urllib.parse.urlencode(query)}"

    def get(self, service: str, path: str, params: dict[str, str]) -> Json:
        request = urllib.request.Request(self.url(service, path, params), headers={"User-Agent": USER_AGENT})
        try:
            with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
                body = response.read()
        except urllib.error.HTTPError as error:
            raise ToolError(f"Open-Meteo refused the request: {self.scrub(reason(error.read()) or str(error))}") \
                from None
        except OSError as error:
            raise ToolError(f"cannot reach Open-Meteo: {self.scrub(str(error))}") from None
        parsed = json.loads(body)
        if not isinstance(parsed, dict):
            raise ToolError("Open-Meteo sent an unexpected reply")
        if parsed.get("error"):
            raise ToolError(f"Open-Meteo refused the request: {parsed.get('reason', 'no reason given')}")
        return parsed

    def scrub(self, text: str) -> str:
        return text.replace(self.key, "***") if self.key else text


def reason(body: bytes) -> str:
    try:
        parsed = json.loads(body)
    except ValueError:
        return body.decode(errors="replace").strip()[-300:]
    return str(parsed.get("reason", "")) if isinstance(parsed, dict) else ""


@dataclass(frozen=True)
class Place:
    name: str
    latitude: float
    longitude: float
    region: str = ""
    country: str = ""

    def describe(self) -> Json:
        place: Json = {"name": self.name, "latitude": self.latitude, "longitude": self.longitude}
        if self.region:
            place["region"] = self.region
        if self.country:
            place["country"] = self.country
        return place


def place_from(result: Json) -> Place:
    return Place(str(result.get("name", "")), float(str(result["latitude"])), float(str(result["longitude"])),
                 str(result.get("admin1", "")), str(result.get("country", "")))


def matches(result: Json, qualifiers: list[str]) -> bool:
    """Whether every qualifier ("Quebec", "CA", "Montréal") names the result's region or country."""
    fields = [str(result.get(key, "")).casefold()
              for key in ("admin1", "admin2", "admin3", "admin4", "country", "country_code")]
    return all(any(field and (field == wanted or field.startswith(wanted)) for field in fields)
               for wanted in (qualifier.casefold() for qualifier in qualifiers))


def locate(api: Api, location: str) -> tuple[Place, list[Json], str]:
    """The place a location names, the other places it could have meant, and a note when
    the region or country given matched none of them."""
    found = COORDINATES.match(location)
    if found:
        latitude, longitude = float(found.group(1)), float(found.group(2))
        if abs(latitude) > 90 or abs(longitude) > 180:
            raise ToolError(f"{location!r} is not a valid latitude,longitude")
        return Place(f"{latitude}, {longitude}", latitude, longitude), [], ""
    name, *qualifiers = [part.strip() for part in location.split(",") if part.strip()] or [""]
    if len(name) < 2:
        raise ToolError("location must be a place name (\"Laval, Quebec\") or latitude,longitude")
    reply = api.get("geocoding-api", "/v1/search", {"name": name, "count": "10", "language": "en", "format": "json"})
    raw = reply.get("results")
    results = [result for result in raw if isinstance(result, dict)] if isinstance(raw, list) else []
    if not results:
        raise ToolError(f"no place named {name!r} found; try another spelling, a nearby city, or coordinates")
    narrowed = [result for result in results if matches(result, qualifiers)]
    note = ""
    if not narrowed:
        # Abbreviations such as "QC" or "TX" name no field: fall back to the most
        # prominent place of that name, say so, and list the others.
        narrowed = results
        note = (f"{', '.join(qualifiers)!r} matched no region or country; this is the first {name!r} found. "
                "If it is the wrong one, ask again with the full region or country name, or coordinates")
    chosen, *others = narrowed
    return place_from(chosen), [place_from(other).describe() for other in others[:5 if note else 3]], note


def with_place_notes(result: Json, others: list[Json], note: str) -> Json:
    if note:
        result["location_note"] = note
    if others:
        result["other_places_with_this_name"] = others
    return result


def weather_name(code: object) -> object:
    return WEATHER_CODES.get(code, f"code {code}") if isinstance(code, int) else code


def rows(block: object, key: str) -> list[Json]:
    """Open-Meteo's columns ({"time": [...], "temperature_2m": [...]}) as one row per time."""
    if not isinstance(block, dict):
        return []
    columns = {name: values for name, values in block.items() if isinstance(values, list)}
    times = columns.pop("time", [])
    result: list[Json] = []
    for index, time in enumerate(times):
        row: Json = {key: time}
        for name, values in columns.items():
            value = values[index] if index < len(values) else None
            row["weather" if "weather_code" == name else name] = \
                weather_name(value) if "weather_code" == name else value
        result.append(row)
    return result


def has_data(row: Json) -> bool:
    return any(value is not None for name, value in row.items() if name not in ("date", "time"))


def units_of(reply: Json, *blocks: str) -> Json:
    units: Json = {}
    for block in blocks:
        found = reply.get(f"{block}_units")
        if isinstance(found, dict):
            units.update({name: unit for name, unit in found.items()
                          if name not in ("time", "interval", "weather_code") and unit not in ("iso8601", "unixtime")})
    return units


def forecast(api: Api, location: str, days: int, hourly: bool, units: str) -> Json:
    if not 1 <= days <= MAX_FORECAST_DAYS:
        raise ToolError(f"days must be 1 to {MAX_FORECAST_DAYS}")
    if hourly and days > MAX_HOURLY_FORECAST_DAYS:
        raise ToolError(f"with hourly, days must be at most {MAX_HOURLY_FORECAST_DAYS}")
    place, others, note = locate(api, location)
    params = {"latitude": str(place.latitude), "longitude": str(place.longitude), "timezone": "auto",
              "forecast_days": str(days), "current": ",".join(CURRENT), "daily": ",".join(FORECAST_DAILY),
              **UNITS[units]}
    if hourly:
        params["hourly"] = ",".join(FORECAST_HOURLY)
    reply = api.get("api", "/v1/forecast", params)
    current = reply.get("current")
    result: Json = {"location": place.describe(), "timezone": reply.get("timezone")}
    if isinstance(current, dict):
        result["current"] = {("weather" if "weather_code" == name else name):
                             (weather_name(value) if "weather_code" == name else value)
                             for name, value in current.items() if "interval" != name}
    result["daily"] = rows(reply.get("daily"), "date")
    if hourly:
        result["hourly"] = rows(reply.get("hourly"), "time")
    result["units"] = units_of(reply, "current", "daily", *(["hourly"] if hourly else []))
    return with_place_notes(result, others, note)


def utc_today() -> date:
    """Today in UTC: the archive's days run a little behind in every time zone anyway."""
    return datetime.now(timezone.utc).date()


def parse_day(text: str, what: str) -> date:
    try:
        return date.fromisoformat(text.strip())
    except ValueError:
        raise ToolError(f"{what} must be a date like 2024-07-15, got {text!r}") from None


def history(api: Api, location: str, start_text: str, end_text: str | None, hourly: bool, units: str,
            today: date | None = None) -> Json:
    today = today or utc_today()
    start = parse_day(start_text, "start")
    end = parse_day(end_text, "end") if end_text else start
    if end < start:
        raise ToolError("end is before start")
    if start < FIRST_HISTORY_DAY:
        raise ToolError(f"the record starts on {FIRST_HISTORY_DAY}")
    if start >= today:
        raise ToolError("start must be in the past; use weather_forecast for today and later")
    end = min(end, today - timedelta(days=1))
    span = (end - start).days + 1
    limit = MAX_HOURLY_HISTORY_DAYS if hourly else MAX_HISTORY_DAYS
    if span > limit:
        raise ToolError(f"at most {limit} days per call{' with hourly' if hourly else ''}; split the period")
    place, others, note = locate(api, location)
    params = {"latitude": str(place.latitude), "longitude": str(place.longitude), "timezone": "auto",
              "start_date": start.isoformat(), "end_date": end.isoformat(), "daily": ",".join(HISTORY_DAILY),
              **UNITS[units]}
    if hourly:
        params["hourly"] = ",".join(HISTORY_HOURLY)
    reply = api.get("archive-api", "/v1/archive", params)
    daily = rows(reply.get("daily"), "date")
    result: Json = {"location": place.describe(), "timezone": reply.get("timezone"),
                    "source": "Open-Meteo historical weather (ERA5 reanalysis, about 9 to 25 km grid)",
                    "daily": [row for row in daily if has_data(row)]}
    if hourly:
        result["hourly"] = [row for row in rows(reply.get("hourly"), "time") if has_data(row)]
    missing = [str(row["date"]) for row in daily if not has_data(row)]
    if missing:
        result["note"] = (f"no data yet for {missing[0]}{f' to {missing[-1]}' if len(missing) > 1 else ''}: "
                          "the archive lags a few days behind")
    result["units"] = units_of(reply, "daily", *(["hourly"] if hourly else []))
    return with_place_notes(result, others, note)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("forecast", "history"):
        command = sub.add_parser(name)
        command.add_argument("--location", required=True)
        command.add_argument("--hourly", action="store_true")
        command.add_argument("--units", choices=sorted(UNITS), default="metric")
    sub.choices["forecast"].add_argument("--days", type=int, default=3)
    sub.choices["history"].add_argument("--start", required=True)
    sub.choices["history"].add_argument("--end")
    return parser


def main() -> int:
    args = build_parser().parse_args()
    api = Api.from_env(dict(os.environ))
    try:
        if "forecast" == args.command:
            result = forecast(api, args.location, args.days, args.hourly, args.units)
        else:
            result = history(api, args.location, args.start, args.end, args.hourly, args.units)
    except ToolError as error:
        print(f"Error: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
