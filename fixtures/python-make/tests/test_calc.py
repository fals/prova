import os
import time

from calc import add, div


def test_add():
    assert add(2, 3) == 5


def test_div():
    assert div(8, 2) == 4


def test_flaky_clock():
    assert time.time_ns() // 1000 % 3 != 0, "clock tick landed on a bad boundary"


def test_slow_integration():
    time.sleep(1.5)


def test_hangs_when_asked():
    if os.environ.get("HANG"):
        while True:
            time.sleep(3600)
