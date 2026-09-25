# pandas (the manylinux wheel from PyPI, with numpy's) under wasm-env,
# compared with qemu by tests/ubuntu.sh.
import io

import numpy as np
import pandas as pd

print("pandas", pd.__version__)
df = pd.DataFrame({
    "city": ["Oslo", "Lima", "Oslo", "Pune", "Lima", "Pune", "Oslo"],
    "temp": [3.5, 19.0, -1.25, 31.0, 21.5, 29.75, 0.5],
    "rain": [12, 0, 30, 2, 1, None, 7],
})
print(df)
print(df.dtypes.to_dict())
print(df.describe().round(4))

g = df.groupby("city")
print(g["temp"].agg(["mean", "min", "max", "count"]).round(4))
print(g["rain"].sum(), g.size().to_dict())

other = pd.DataFrame({"city": ["Lima", "Oslo", "Rome"], "country": ["PE", "NO", "IT"]})
print(df.merge(other, on="city", how="left").sort_values(["city", "temp"]).to_string())

print(df.fillna({"rain": -1}).astype({"rain": "int64"})["rain"].tolist())
print(df.dropna().shape, df["rain"].isna().sum())
print(df.sort_values("temp", ascending=False).head(3).reset_index(drop=True))

wide = df.assign(n=range(len(df))).pivot_table(index="city", values=["temp", "n"], aggfunc="sum")
print(wide)

s = pd.Series(["apple pie", "Banana", "cherry tart", None, "date"])
print(s.str.upper().tolist(), s.str.len().tolist(), s.str.contains("an").tolist())
print(s.str.split(" ").str[0].tolist(), s.str.replace("a", "@", regex=False).tolist())

idx = pd.date_range("2024-02-27", periods=6, freq="D")
ts = pd.Series(np.arange(6.0) ** 2, index=idx)
print(ts)
print(ts.resample("2D").sum())
print(ts.rolling(3).mean().round(4).tolist())
print(idx.day_name().tolist(), idx.is_leap_year.tolist())
utc = pd.Timestamp("2024-03-10 06:30", tz="UTC")
print(utc.tz_convert("America/New_York"), utc.tz_convert("Asia/Kolkata"))
print(pd.to_datetime(["2024-01-05", "2023-12-31 23:59:59"], format="ISO8601").strftime("%Y/%m/%d %H:%M").tolist())
print((pd.Timestamp("2024-03-01") - pd.Timestamp("2023-03-01")).days)

cat = pd.Categorical(["lo", "hi", "mid", "lo", "hi"], categories=["lo", "mid", "hi"], ordered=True)
print(pd.Series(cat).value_counts().sort_index().to_dict(), cat.max(), cat.codes.tolist())

buf = io.StringIO()
df.to_csv(buf, index=False)
back = pd.read_csv(io.StringIO(buf.getvalue()))
print(buf.getvalue().splitlines()[:3], back.equals(df))
print(pd.read_csv(io.StringIO("a;b\n1;x\n2;y\n"), sep=";").to_dict("list"))
print(df.to_json(orient="records")[:80])

big = pd.DataFrame({"k": np.arange(20000) % 97, "v": np.arange(20000, dtype=np.float64) / 7})
agg = big.groupby("k")["v"].sum()
print(round(float(agg.sum()), 6), agg.idxmax(), round(float(big["v"].quantile(0.9)), 6))
print(big.sort_values("v", ascending=False)["k"].head(5).tolist(), big["k"].nunique())
print(pd.concat([df.head(2), df.tail(2)]).index.tolist())
print(df.set_index("city").loc["Oslo", "temp"].tolist(), df.iloc[2:5, 1].tolist())
print(df.apply(lambda r: f"{r['city']}:{r['temp']}", axis=1).tolist()[:3])
print(pd.cut(df["temp"], bins=[-10, 0, 20, 40]).astype(str).tolist())
