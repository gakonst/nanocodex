#include "agent_cursor.hpp"
#include <cassert>
#include <chrono>
#include <iostream>
using namespace cua::hyprland;
using namespace std::chrono_literals;
int main() {
    AgentCursorState a, b;
    const auto t = AgentCursorState::Clock::now();
    assert(a.alpha(t) == 0);
    a.move(12, 34, t); b.move(56, 78, t);
    a.press(t + 100ms);
    assert(a.x == 12 && b.x == 56 && a.y == 34 && b.y == 78);
    assert(a.alpha(t + 1000ms) == 1);
    assert(a.alpha(t + 1150ms) == 0.5);
    assert(a.alpha(t + 1300ms) == 0);
    assert(a.pulse(t + 100ms) == 1 && b.pulse(t + 100ms) == 0);
    assert(a.pulse(t + 350ms) == 0);
    a.visible = false;
    assert(a.alpha(t) == 0 && a.pulse(t) == 0 && b.alpha(t) == 1);
    const auto start = AgentCursorState::Clock::now();
    volatile double sum = 0;
    for (int i = 0; i < 1000000; ++i) {
        b.move(i % 100, i % 200, t);
        sum = sum + b.alpha(t + std::chrono::milliseconds(i % 1500)) + b.pulse(t);
    }
    std::cout << "1M cursor updates ms=" << std::chrono::duration<double, std::milli>(AgentCursorState::Clock::now() - start).count() << " checksum=" << sum << '\n';
}
