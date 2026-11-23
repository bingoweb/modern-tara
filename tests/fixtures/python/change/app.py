import asyncio
import os
from typing import List  # OLD:UP035:needs-newer

untouched = os.path.join("x", "y")  # UNCHANGED


def old(xs: List[int]) -> str:  # OLD:UP006:needs-newer
    return os.path.join("a", "b")  # OLD:PTH118:modern


def decoy(xs: list) -> str:
    return f"{xs}"  # DECOY


async def run(a, b):
    await asyncio.gather(a, b)  # OLD:python-asyncio-gather:needs-newer


def shorter(items, d):
    for i, x in enumerate(items):
        print(items[i])  # OLD:PLR1736:modern
    for i, x in enumerate(items):
        print(x)  # DECOY
    for k in d:
        print(d[k])  # DECOY
    first = list(items)[0]  # OLD:RUF015:modern
    kept = next(iter(items))  # DECOY
    copy = list([x for x in items])  # OLD:C4:modern
    table = {"a": 1}
    for _k, v in table.items():  # OLD:PERF102:modern
        print(v)
    return first, kept, copy
