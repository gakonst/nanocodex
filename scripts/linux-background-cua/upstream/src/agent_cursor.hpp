#pragma once

#include <algorithm>
#include <chrono>

namespace cua::hyprland {
// Pure timing state: no primary cursor/focus APIs, and no input authority.
struct AgentCursorState {
    using Clock = std::chrono::steady_clock;
    double x = 0, y = 0;
    Clock::time_point activity{}, click{};
    bool visible = false;
    void move(double px, double py, Clock::time_point now) {
        x = px; y = py; activity = now; visible = true;
    }
    void press(Clock::time_point now) { click = now; activity = now; }
    double alpha(Clock::time_point now) const {
        if (!visible) return 0;
        const auto age = std::chrono::duration<double, std::milli>(now - activity).count();
        return std::clamp((1200.0 - age) / 300.0, 0.0, 1.0);
    }
    double pulse(Clock::time_point now) const {
        if (!visible || click == Clock::time_point{}) return 0;
        const auto age = std::chrono::duration<double, std::milli>(now - click).count();
        return std::clamp(1.0 - age / 250.0, 0.0, 1.0);
    }
};
}
