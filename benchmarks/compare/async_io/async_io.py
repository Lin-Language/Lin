# async_io.py — I/O-bound concurrency via asyncio + Semaphore(50) bounding 200
# coroutines, each `asyncio.sleep(0.05)` then return i*2+1. Prints "RESULT=<int>".
import asyncio

TASKS = 200
SLEEP_MS = 50
CONCURRENCY = 50


async def task(i, sem):
    async with sem:
        await asyncio.sleep(SLEEP_MS / 1000.0)
        return i * 2 + 1


async def run():
    sem = asyncio.Semaphore(CONCURRENCY)
    results = await asyncio.gather(*(task(i, sem) for i in range(TASKS)))
    return sum(results)


def main():
    total = asyncio.run(run())
    print(f"RESULT={total}")


if __name__ == "__main__":
    main()
