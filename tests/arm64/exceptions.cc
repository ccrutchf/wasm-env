// C++ exceptions through translated code: throwing across frames, catching
// by type and by base class, rethrowing, destructors running during
// unwinding, exceptions from the standard library, nested try blocks, and
// std::exception_ptr across a thread.
#include <cstdio>
#include <memory>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

static int destroyed;
struct Guard {
	int id;
	~Guard() { destroyed += id; }
};

struct Base {
	virtual ~Base() = default;
	virtual const char *what() const { return "base"; }
};
struct Derived : Base {
	const char *what() const override { return "derived"; }
};

__attribute__((noinline)) static int deep(int n)
{
	Guard g{n};
	if (n == 0)
		throw std::runtime_error("from the bottom");
	return deep(n - 1) + 1;
}

__attribute__((noinline)) static void throws_derived() { throw Derived(); }

int main()
{
	try {
		deep(10);
	} catch (const std::exception &e) {
		std::printf("caught: %s; destructors ran: %d\n", e.what(), destroyed);
	}
	try {
		throws_derived();
	} catch (const Base &b) {
		std::printf("caught by base: %s\n", b.what());
	}
	try {
		try {
			throw 42;
		} catch (int v) {
			std::printf("inner: %d\n", v);
			throw;
		}
	} catch (int v) {
		std::printf("rethrown: %d\n", v);
	}
	try {
		std::vector<int> v(3);
		v.at(10) = 1;
	} catch (const std::out_of_range &) {
		std::printf("std::out_of_range from the library\n");
	}
	try {
		std::string s;
		s.reserve(s.max_size() + 1);
	} catch (const std::length_error &) {
		std::printf("std::length_error\n");
	}
	std::exception_ptr err;
	std::thread t([&] {
		try {
			deep(3);
		} catch (...) {
			err = std::current_exception();
		}
	});
	t.join();
	try {
		std::rethrow_exception(err);
	} catch (const std::runtime_error &e) {
		std::printf("across a thread: %s\n", e.what());
	}
	int caught = 0;
	for (int i = 0; i < 1000; i++) {
		try {
			if (i % 2)
				throw i;
		} catch (int) {
			caught++;
		}
	}
	std::printf("caught in a loop: %d\n", caught);
	return 0;
}
