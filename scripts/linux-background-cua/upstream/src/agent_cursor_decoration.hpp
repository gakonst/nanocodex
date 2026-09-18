#pragma once
#include "agent_cursor.hpp"
#include <array>
#include <memory>
#include <src/desktop/view/Window.hpp>
#include <src/render/decorations/IHyprWindowDecoration.hpp>
#include <src/render/pass/RectPassElement.hpp>
#include <src/render/Renderer.hpp>
#include <src/output/Monitor.hpp>

namespace cua::hyprland {
// A decoration is inserted with its owning window in the compositor render
// pass. Higher windows and popups occlude it; it has no input region or seat.
class AgentCursorDecoration final : public IHyprWindowDecoration {
    PHLWINDOWREF window_;
    std::shared_ptr<AgentCursorState> state_;
    unsigned lane_;
    std::shared_ptr<int> lifetime_ = std::make_shared<int>(0);
  public:
    AgentCursorDecoration(PHLWINDOW window, std::shared_ptr<AgentCursorState> state, unsigned lane)
        : IHyprWindowDecoration(window), window_(window), state_(std::move(state)), lane_(lane) {}
    std::weak_ptr<int> lifetime() const { return lifetime_; }
    SDecorationPositioningInfo getPositioningInfo() override { return {}; }
    void onPositioningReply(const SDecorationPositioningReply&) override {}
    eDecorationType getDecorationType() override { return DECORATION_CUSTOM; }
    eDecorationLayer getDecorationLayer() override { return DECORATION_LAYER_OVER; }
    uint64_t getDecorationFlags() override { return DECORATION_NON_SOLID; }
    std::string getDisplayName() override { return "Nanocodex agent cursor"; }
    void updateWindow(PHLWINDOW) override { damageEntire(); }
    void damageEntire() override {
        if (const auto window = window_.lock())
            if (const auto box = window->surfaceLogicalBox())
                g_pHyprRenderer->damageBox(CBox{box->x + state_->x - 18, box->y + state_->y - 18, 36, 36});
    }
    void draw(PHLMONITOR monitor, float const& window_alpha) override {
        const auto window = window_.lock();
        const auto opacity = state_->alpha(AgentCursorState::Clock::now()) * window_alpha;
        if (!window || !monitor || opacity <= 0 || window->isHidden()) return;
        const auto surface = window->surfaceLogicalBox();
        if (!surface) return;
        const auto now = AgentCursorState::Clock::now();
        auto clip = *surface;
        clip.translate(-monitor->m_position).scale(monitor->m_scale);
        const auto rectangle = [&](double x, double y, double w, double h, CHyprColor color, int round) {
            CBox box{surface->x + state_->x + x, surface->y + state_->y + y, w, h};
            box.translate(-monitor->m_position).scale(monitor->m_scale);
            color.a *= opacity;
            CRectPassElement::SRectData data;
            data.box = box; data.color = color; data.round = round * monitor->m_scale; data.clipBox = clip;
            g_pHyprRenderer->m_renderPass.add(makeUnique<CRectPassElement>(data));
        };
        const std::array<CHyprColor, 8> palette{{
            {0.15F, 0.85F, 1.F, 1.F}, {1.F, 0.45F, 0.8F, 1.F},
            {0.4F, 1.F, 0.4F, 1.F}, {1.F, 0.7F, 0.2F, 1.F},
            {0.6F, 0.5F, 1.F, 1.F}, {1.F, 0.35F, 0.3F, 1.F},
            {0.3F, 1.F, 0.75F, 1.F}, {1.F, 1.F, 0.4F, 1.F}}};
        const auto color = palette[lane_ % palette.size()];
        if (const auto pulse = state_->pulse(now); pulse > 0) {
            const auto radius = 8 + (1 - pulse) * 8;
            rectangle(-radius, -radius, radius * 2, radius * 2, color.modifyA(pulse * 0.4), radius);
        }
        // High-contrast crosshair; its intersection is the exact injected point.
        rectangle(-7, -2, 15, 5, CHyprColor{0.F, 0.F, 0.F, 0.9F}, 1);
        rectangle(-2, -7, 5, 15, CHyprColor{0.F, 0.F, 0.F, 0.9F}, 1);
        rectangle(-6, -1, 13, 3, color, 1);
        rectangle(-1, -6, 3, 13, color, 1);
    }
};
}
