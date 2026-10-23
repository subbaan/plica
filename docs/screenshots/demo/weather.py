#!/usr/bin/env python3
"""Tiny weather log: read readings.csv and print a summary."""

import csv
from dataclasses import dataclass
from statistics import mean


@dataclass
class Reading:
    day: str
    temp_c: float
    rain_mm: float = 0.0


def load(path: str) -> list[Reading]:
    with open(path, newline="") as f:
        rows = csv.DictReader(f)
        return [Reading(r["day"], float(r["temp"]), float(r["rain"])) for r in rows]


def summary(readings: list[Reading]) -> str:
    # TODO: weekly totals
    warm = [r.day for r in readings if r.temp_c > 20]
    avg = mean(r.temp_c for r in readings)
    return f"{len(readings)} days, mean {avg:.1f} °C, warm: {', '.join(warm) or 'none'}"


if __name__ == "__main__":
    print(summary(load("readings.csv")))
