#!/usr/bin/env python3
"""Tests for the weather plugin, against a fake Open-Meteo. Run: python3 -m unittest -v test_weather_tool.py

WEATHER_LIVE_TEST=1 also runs check_config.py's checks against the real Open-Meteo."""

import os
import unittest
from dataclasses import dataclass, field
from datetime import date

import weather_tool as tool
from weather_tool import Api, Json, ToolError

MONTREAL: Json = {"name": "Montreal", "latitude": 45.5, "longitude": -73.6, "admin1": "Quebec",
                  "country": "Canada", "country_code": "CA"}
MONTREAL_WI: Json = {"name": "Montreal", "latitude": 46.4, "longitude": -90.2, "admin1": "Wisconsin",
                     "country": "United States", "country_code": "US"}


@dataclass(frozen=True)
class FakeApi(Api):
    """Answers from canned replies by service, and records what was asked."""

    replies: dict[str, Json] = field(default_factory=dict)
    calls: list[tuple[str, dict[str, str]]] = field(default_factory=list)

    def get(self, service: str, path: str, params: dict[str, str]) -> Json:
        self.calls.append((service, params))
        return self.replies[service]


def geocoding(*results: Json) -> Json:
    return {"results": list(results)}


class LocateTests(unittest.TestCase):

    def test_coordinates_skip_the_lookup_and_may_be_negative(self) -> None:
        api = FakeApi()
        place, others, note = tool.locate(api, "-33.87, 151.21")
        self.assertEqual((place.latitude, place.longitude), (-33.87, 151.21))
        self.assertEqual(([], ""), (others, note))
        with self.assertRaises(ToolError):
            tool.locate(api, "95,10")

    def test_a_region_picks_among_places_of_the_same_name(self) -> None:
        api = FakeApi(replies={"geocoding-api": geocoding(MONTREAL, MONTREAL_WI)})
        place, others, note = tool.locate(api, "Montreal, Wisconsin")
        self.assertEqual("Wisconsin", place.region)
        self.assertEqual(([], ""), (others, note))
        self.assertEqual("Montreal", api.calls[0][1]["name"])

    def test_country_code_and_prefix_match(self) -> None:
        api = FakeApi(replies={"geocoding-api": geocoding(MONTREAL_WI, MONTREAL)})
        self.assertEqual("Quebec", tool.locate(api, "Montreal, CA")[0].region)
        self.assertEqual("Quebec", tool.locate(api, "Montreal, queb")[0].region)

    def test_an_unmatched_region_falls_back_with_a_note_and_the_alternatives(self) -> None:
        api = FakeApi(replies={"geocoding-api": geocoding(MONTREAL, MONTREAL_WI)})
        place, others, note = tool.locate(api, "Montreal, QC")
        self.assertEqual("Quebec", place.region)
        self.assertIn("'QC' matched no region", note)
        self.assertEqual("Wisconsin", others[0]["region"])

    def test_unknown_place(self) -> None:
        with self.assertRaisesRegex(ToolError, "no place named 'Xqzzy'"):
            tool.locate(FakeApi(replies={"geocoding-api": {}}), "Xqzzy")


class ForecastTests(unittest.TestCase):

    def reply(self) -> Json:
        return {
            "timezone": "America/Toronto",
            "current_units": {"time": "iso8601", "interval": "seconds", "temperature_2m": "°C"},
            "current": {"time": "2026-09-29T17:45", "interval": 900, "temperature_2m": 14.0, "weather_code": 1},
            "daily_units": {"time": "iso8601", "temperature_2m_max": "°C", "weather_code": "wmo code"},
            "daily": {"time": ["2026-09-29", "2026-09-30"], "weather_code": [3, 95],
                      "temperature_2m_max": [14.3, 17.4]},
        }

    def test_columns_become_rows_with_named_weather(self) -> None:
        api = FakeApi(replies={"geocoding-api": geocoding(MONTREAL), "api": self.reply()})
        result = tool.forecast(api, "Montreal", 2, False, "metric")
        self.assertEqual({"time": "2026-09-29T17:45", "temperature_2m": 14.0, "weather": "mainly clear"},
                         result["current"])
        self.assertEqual([{"date": "2026-09-29", "weather": "overcast", "temperature_2m_max": 14.3},
                          {"date": "2026-09-30", "weather": "thunderstorm", "temperature_2m_max": 17.4}],
                         result["daily"])
        self.assertEqual({"temperature_2m": "°C", "temperature_2m_max": "°C"}, result["units"])
        self.assertNotIn("hourly", api.calls[1][1])

    def test_imperial_units_and_hourly_are_asked_for(self) -> None:
        api = FakeApi(replies={"api": self.reply()})
        tool.forecast(api, "45.5,-73.6", 3, True, "imperial")
        params = api.calls[0][1]
        self.assertEqual(("fahrenheit", "mph", "inch", "3"), (params["temperature_unit"], params["wind_speed_unit"],
                                                            params["precipitation_unit"], params["forecast_days"]))
        self.assertIn("hourly", params)

    def test_hourly_is_a_table_from_the_current_hour_on(self) -> None:
        reply = {**self.reply(), "hourly": {"time": ["2026-09-29T16:00", "2026-09-29T17:00", "2026-09-29T18:00"],
                                            "precipitation": [0.0, 0.1, 0.4], "weather_code": [3, 51, 61]}}
        api = FakeApi(replies={"api": reply})
        result = tool.forecast(api, "45.5,-73.6", 1, True, "metric")
        self.assertEqual({"columns": ["time", "precipitation", "weather"],
                          "rows": [["2026-09-29T17:00", 0.1, "light drizzle"],
                                   ["2026-09-29T18:00", 0.4, "slight rain"]]},
                         result["hourly"])

    def test_limits_are_checked_before_any_request(self) -> None:
        api = FakeApi()
        for days, hourly in ((0, False), (17, False), (4, True)):
            with self.assertRaises(ToolError):
                tool.forecast(api, "Montreal", days, hourly, "metric")
        self.assertEqual([], api.calls)


class HistoryTests(unittest.TestCase):
    TODAY = date(2026, 9, 29)

    def reply(self) -> Json:
        return {"timezone": "America/Toronto",
                "daily": {"time": ["2026-09-26", "2026-09-27", "2026-09-28"], "weather_code": [63, None, None],
                          "precipitation_sum": [6.2, None, None]}}

    def test_days_without_data_yet_are_dropped_and_noted(self) -> None:
        api = FakeApi(replies={"archive-api": self.reply()})
        result = tool.history(api, "45.5,-73.6", "2026-09-26", "2026-09-28", False, "metric", self.TODAY)
        self.assertEqual([{"date": "2026-09-26", "weather": "moderate rain", "precipitation_sum": 6.2}],
                         result["daily"])
        self.assertIn("no data yet for 2026-09-27 to 2026-09-28", str(result["note"]))

    def test_end_defaults_to_start_and_is_capped_at_yesterday(self) -> None:
        api = FakeApi(replies={"archive-api": self.reply()})
        tool.history(api, "45.5,-73.6", "2026-09-20", None, False, "metric", self.TODAY)
        tool.history(api, "45.5,-73.6", "2026-09-20", "2026-12-01", False, "metric", self.TODAY)
        calls = api.calls
        self.assertEqual("2026-09-20", calls[0][1]["end_date"])
        self.assertEqual("2026-09-28", calls[1][1]["end_date"])

    def test_bad_ranges(self) -> None:
        api = FakeApi()
        for start, end, hourly, error in (("2026-09-29", None, False, "weather_forecast"),
                                          ("1939-12-31", None, False, "1940"),
                                          ("2026-09-10", "2026-09-01", False, "before start"),
                                          ("2024-01-01", "2025-06-01", False, "at most 366"),
                                          ("2026-09-01", "2026-09-08", True, "at most 7 days per call with hourly"),
                                          ("15/07/2024", None, False, "like 2024-07-15")):
            with self.assertRaisesRegex(ToolError, error):
                tool.history(api, "45.5,-73.6", start, end, hourly, "metric", self.TODAY)


class LocationTests(unittest.TestCase):

    def test_no_location_or_here_means_the_owners_default(self) -> None:
        env = {"WEATHER_DEFAULT_LOCATION": "Laval, Quebec"}
        for given in (None, "", " here ", "Home", "my location"):
            self.assertEqual("Laval, Quebec", tool.resolve_location(given, env))
        self.assertEqual("Paris, France", tool.resolve_location("Paris, France", env))
        with self.assertRaisesRegex(ToolError, "ask the owner which city"):
            tool.resolve_location(None, {})

    def test_words_that_name_no_place_are_refused(self) -> None:
        for vague in ("global", "World", "everywhere"):
            with self.assertRaisesRegex(ToolError, "is not a place"):
                tool.resolve_location(vague, {"WEATHER_DEFAULT_LOCATION": "Laval, Quebec"})


class ApiTests(unittest.TestCase):

    def test_free_and_customer_servers(self) -> None:
        self.assertEqual("https://api.open-meteo.com/v1/forecast?a=1", Api().url("api", "/v1/forecast", {"a": "1"}))
        keyed = Api("s3cret")
        self.assertEqual("https://customer-archive-api.open-meteo.com/v1/archive?a=1&apikey=s3cret",
                         keyed.url("archive-api", "/v1/archive", {"a": "1"}))
        self.assertEqual("bad key *** refused", keyed.scrub("bad key s3cret refused"))

    def test_error_reason_from_a_json_or_text_body(self) -> None:
        self.assertEqual("out of range", tool.reason(b'{"error": true, "reason": "out of range"}'))
        self.assertEqual("Bad Gateway", tool.reason(b"Bad Gateway"))


@unittest.skipUnless("1" == os.environ.get("WEATHER_LIVE_TEST"), "set WEATHER_LIVE_TEST=1 to call Open-Meteo")
class LiveTests(unittest.TestCase):

    def test_check_config_passes(self) -> None:
        from check_config import live_checks
        from weather_tool import utc_today
        checks = live_checks(Api.from_env(dict(os.environ)), "Montreal, Quebec", utc_today())
        self.assertTrue(all(check.ok for check in checks), checks)


if __name__ == "__main__":
    unittest.main()
