# Exercises Python extension modules loaded with dlopen (hashlib via
# OpenSSL, sqlite3, zlib, _decimal, _json, _struct, math, ...). Run by
# tests/python.sh under wasm-env and under qemu; the outputs must match.
import math, json, struct, hashlib, datetime, decimal, re, collections, random, zlib, sqlite3, statistics, itertools
print("math:", math.sqrt(2), math.factorial(20), math.gcd(462, 1071))
print("json:", json.dumps({"a": [1, 2.5, None, True], "b": "é"}, sort_keys=True))
print("struct:", struct.pack("<iQd", -1, 2**40, 1.5).hex())
print("sha256:", hashlib.sha256(b"wasm-env").hexdigest())
print("md5:", hashlib.md5(b"wasm-env").hexdigest())
print("date:", datetime.date(2024, 2, 29) + datetime.timedelta(days=366))
print("decimal:", decimal.Decimal(1) / decimal.Decimal(7))
print("re:", re.findall(r"(\w+)@(\w+)\.com", "a@b.com, c@d.com"))
print("counter:", collections.Counter("mississippi").most_common(2))
random.seed(42); print("random:", [random.randint(1, 100) for _ in range(5)])
print("zlib:", len(zlib.compress(b"x" * 10000)), zlib.crc32(b"hello"))
db = sqlite3.connect(":memory:")
db.execute("create table t (x integer, y text)")
db.executemany("insert into t values (?, ?)", [(i, str(i * i)) for i in range(10)])
print("sqlite:", db.execute("select sum(x), group_concat(y) from t where x % 2 = 0").fetchone(), sqlite3.sqlite_version)
print("stats:", statistics.mean([1, 2, 3, 4.5]), statistics.stdev([2, 4, 4, 4, 5, 5, 7, 9]))
print("big:", 2**200 % 1000007, sum(itertools.islice(itertools.count(1), 1000)))
