-- LuaJIT compiling traces as it runs: numeric loops, calls, tables,
-- closures, strings and a little object code. Deterministic output.
local function fib(n) if n < 2 then return n end return fib(n - 1) + fib(n - 2) end

local function sieve(n)
  local composite, count = {}, 0
  for i = 2, n do
    if not composite[i] then
      count = count + 1
      for j = i * i, n, i do composite[j] = true end
    end
  end
  return count
end

local function mandel(size)
  local inside = 0
  for y = 0, size - 1 do
    local ci = 2 * y / size - 1
    for x = 0, size - 1 do
      local cr, zr, zi, n = 2.5 * x / size - 2, 0, 0, 0
      while n < 50 and zr * zr + zi * zi < 4 do
        zr, zi = zr * zr - zi * zi + cr, 2 * zr * zi + ci
        n = n + 1
      end
      if n == 50 then inside = inside + 1 end
    end
  end
  return inside
end

local function strings(n)
  local parts = {}
  for i = 1, n do parts[#parts + 1] = string.format("%d:%s", i, string.rep("x", i % 7)) end
  local s = table.concat(parts, ",")
  local count = 0
  for _ in s:gmatch("xxx") do count = count + 1 end
  return #s, count
end

local Point = {}
Point.__index = Point
function Point.new(x, y) return setmetatable({x = x, y = y}, Point) end
function Point:add(o) return Point.new(self.x + o.x, self.y + o.y) end
local function points(n)
  local p = Point.new(0, 0)
  for i = 1, n do p = p:add(Point.new(i % 3, i % 5)) end
  return p.x, p.y
end

local function closures(n)
  local total = 0
  for i = 1, n do
    local f = function(a) return a * i end
    total = (total + f(3)) % 1000003
  end
  return total
end

local function sorting(n)
  local t, seed = {}, 42
  for i = 1, n do seed = (seed * 1103515245 + 12345) % 2147483648; t[i] = seed end
  table.sort(t)
  return t[1], t[math.floor(n / 2) + 1], t[n]
end

print("fib", fib(27))
print("sieve", sieve(2000000))
print("mandel", mandel(400))
print("strings", strings(200000))
print("points", points(1000000))
print("closures", closures(2000000))
print("sorting", sorting(200000))
print("jit", jit and jit.status and (jit.status() and "on" or "off"))
