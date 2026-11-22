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
