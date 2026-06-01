# pipeline.py — range -> map -> filter -> reduce, materializing each stage
# (list(map(...)) / list(filter(...)) so no lazy fusion). Prints "RESULT=<int>".
N = 2000000


def main():
    a = list(range(0, N))
    b = list(map(lambda x: x * 2, a))
    c = list(filter(lambda x: x % 3 == 0, b))
    total = 0
    for x in c:
        total += x
    print(f"RESULT={total}")


if __name__ == "__main__":
    main()
