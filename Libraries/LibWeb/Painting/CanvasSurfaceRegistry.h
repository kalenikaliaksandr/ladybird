/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/HashMap.h>
#include <AK/Noncopyable.h>
#include <AK/NonnullRefPtr.h>
#include <LibGfx/PaintingSurface.h>
#include <LibWeb/Painting/DisplayListResourceIds.h>

namespace Web::Painting {

class CanvasSurfaceRegistry {
    AK_MAKE_NONCOPYABLE(CanvasSurfaceRegistry);
    AK_MAKE_DEFAULT_MOVABLE(CanvasSurfaceRegistry);

public:
    CanvasSurfaceRegistry() = default;
    ~CanvasSurfaceRegistry() = default;

    CanvasId allocate_canvas_id()
    {
        return CanvasId { m_next_canvas_id++ };
    }

    CanvasId create_canvas_surface(NonnullRefPtr<Gfx::PaintingSurface> surface)
    {
        auto id = allocate_canvas_id();
        set_canvas_surface(id, move(surface));
        return id;
    }

    void set_canvas_surface(CanvasId id, NonnullRefPtr<Gfx::PaintingSurface> surface)
    {
        // The new surface replaces the previous producer's, so a removal deferred
        // while that surface stayed pinned is superseded here — not at claim time,
        // where a failing context creation would lose the flag and orphan the
        // deferred surface.
        if (auto reservation = m_reservations.find(id); reservation != m_reservations.end())
            reservation->value.surface_removal_deferred = false;
        m_surfaces.set(id, move(surface));
    }

    void remove_canvas_surface(CanvasId id)
    {
        if (auto reservation = m_reservations.find(id); reservation != m_reservations.end() && reservation->value.pinned) {
            // A pinned surface is still displayed by a placeholder canvas, so it must
            // survive its producer; the actual removal happens when the pin is dropped.
            reservation->value.surface_removal_deferred = true;
            return;
        }
        m_surfaces.remove(id);
        drop_reservation_if_unused(id);
    }

    Gfx::PaintingSurface const* canvas_surface(CanvasId id) const
    {
        return m_surfaces.get(id).value_or(nullptr);
    }

    CanvasId reserve_canvas_id(u64 nonce)
    {
        auto id = allocate_canvas_id();
        m_reservations.set(id, CanvasIdReservation { .nonce = nonce, .claimed = false, .pinned = false, .surface_removal_deferred = false, .released_by_allocator = false, .claim_window_closed = false, .committed_state = {}, .committed_surface = nullptr });
        return id;
    }

    bool claim_canvas_id(CanvasId id, u64 nonce)
    {
        auto reservation = m_reservations.find(id);
        if (reservation == m_reservations.end() || reservation->value.nonce != nonce || reservation->value.claimed)
            return false;
        reservation->value.claimed = true;
        return true;
    }

    void unclaim_canvas_id(CanvasId id)
    {
        auto reservation = m_reservations.find(id);
        if (reservation == m_reservations.end())
            return;
        reservation->value.claimed = false;
        drop_reservation_if_unused(id);
    }

    bool canvas_id_reservation_matches_nonce(CanvasId id, u64 nonce) const
    {
        auto reservation = m_reservations.find(id);
        return reservation != m_reservations.end() && reservation->value.nonce == nonce;
    }

    bool has_reservation(CanvasId id) const
    {
        return m_reservations.contains(id);
    }

    struct CanvasCommitState {
        Gfx::IntSize logical_size;
        bool origin_clean { true };
    };

    // The producer's last committed logical size and origin-clean state. Kept on
    // the reservation so a watcher registered after the commit can still be
    // brought up to date, and so pixel readbacks report the taint that belongs
    // to the presented surface they read.
    void record_canvas_commit(CanvasId id, Gfx::IntSize logical_size, bool origin_clean)
    {
        auto reservation = m_reservations.find(id);
        if (reservation == m_reservations.end())
            return;
        reservation->value.committed_state = CanvasCommitState { logical_size, origin_clean };
    }

    // A metadata-only commit supersedes the previously presented surface: the
    // producer has none anymore, so it must go away even while a watcher pin
    // would normally defer its removal.
    void drop_canvas_surface_for_commit(CanvasId id)
    {
        if (auto reservation = m_reservations.find(id); reservation != m_reservations.end()) {
            reservation->value.surface_removal_deferred = false;
            reservation->value.committed_surface = nullptr;
        }
        m_surfaces.remove(id);
    }

    // The committed frame of a reserved (placeholder-linked) id. The plain
    // surface slot may hold a producer's live drawing surface (WebGL registers
    // it per command batch, and a re-created 2D context publishes a blank one
    // before its first commit); everything presenting or reading on behalf of a
    // placeholder must resolve this snapshot instead.
    void set_committed_canvas_surface(CanvasId id, NonnullRefPtr<Gfx::PaintingSurface> surface)
    {
        auto reservation = m_reservations.find(id);
        if (reservation == m_reservations.end())
            return;
        reservation->value.committed_surface = move(surface);
    }

    Gfx::PaintingSurface const* committed_canvas_surface(CanvasId id) const
    {
        auto reservation = m_reservations.find(id);
        if (reservation == m_reservations.end())
            return nullptr;
        return reservation->value.committed_surface.ptr();
    }

    // Resolves what a display of this canvas should show: the committed frame
    // for reserved ids, the plain surface otherwise (unreserved element
    // canvases present their live surface by design).
    Gfx::PaintingSurface const* canvas_surface_for_presentation(CanvasId id) const
    {
        if (auto const* committed_surface = committed_canvas_surface(id))
            return committed_surface;
        if (has_reservation(id))
            return nullptr;
        return canvas_surface(id);
    }

    Optional<CanvasCommitState> canvas_commit_state(CanvasId id) const
    {
        auto reservation = m_reservations.find(id);
        if (reservation == m_reservations.end())
            return {};
        return reservation->value.committed_state;
    }

    bool canvas_surface_origin_clean(CanvasId id) const
    {
        auto reservation = m_reservations.find(id);
        if (reservation == m_reservations.end() || !reservation->value.committed_state.has_value())
            return true;
        return reservation->value.committed_state->origin_clean;
    }

    void pin_canvas_surface(CanvasId id)
    {
        auto reservation = m_reservations.find(id);
        if (reservation == m_reservations.end())
            return;
        reservation->value.pinned = true;
    }

    void unpin_canvas_surface(CanvasId id)
    {
        auto reservation = m_reservations.find(id);
        if (reservation == m_reservations.end())
            return;
        reservation->value.pinned = false;
        if (reservation->value.surface_removal_deferred) {
            reservation->value.surface_removal_deferred = false;
            m_surfaces.remove(id);
        }
        drop_reservation_if_unused(id);
    }

    // Reservations are owned by their allocator (the placeholder's process): claim,
    // unclaim, pin and unpin never drop one, so neither a placeholder canvas element
    // being garbage-collected nor a producer-side resize (destroy and recreate with
    // the same id) can invalidate the id the OffscreenCanvas producer still holds.
    // The reservation is dropped once the allocator has released it, no claim or
    // watcher pin keeps it alive, and the claim window is closed — that is, a
    // connection whose death precludes any future claim is gone.
    void release_canvas_id_reservation(CanvasId id)
    {
        auto reservation = m_reservations.find(id);
        if (reservation == m_reservations.end())
            return;
        reservation->value.released_by_allocator = true;
        drop_reservation_if_unused(id);
    }

    // The claim window closes on a producer-side signal only: the connection
    // holding the claim dying, or the link holder declaring no claim will ever
    // come (decline). Releases — element-level and allocator-connection-level
    // alike — keep the window open: the placeholder reference is weak, and the
    // transferred OffscreenCanvas may not even have created its context yet.
    void close_canvas_id_claim_window(CanvasId id)
    {
        auto reservation = m_reservations.find(id);
        if (reservation == m_reservations.end())
            return;
        reservation->value.claim_window_closed = true;
        drop_reservation_if_unused(id);
    }

    // Reclaims a reservation stranded by a producer that vanished without a
    // decline (a terminating worker runs no GC finalizers): only applies once
    // the allocator has released it (the placeholder is gone, so nothing can
    // ever watch commits again) and no producer currently holds a claim.
    void close_claim_window_of_released_unclaimed_reservation(CanvasId id)
    {
        auto reservation = m_reservations.find(id);
        if (reservation == m_reservations.end() || !reservation->value.released_by_allocator || reservation->value.claimed)
            return;
        reservation->value.claim_window_closed = true;
        drop_reservation_if_unused(id);
    }

private:
    struct CanvasIdReservation {
        u64 nonce { 0 };
        bool claimed { false };
        bool pinned { false };
        bool surface_removal_deferred { false };
        bool released_by_allocator { false };
        bool claim_window_closed { false };
        Optional<CanvasCommitState> committed_state;
        RefPtr<Gfx::PaintingSurface> committed_surface;
    };

    void drop_reservation_if_unused(CanvasId id)
    {
        auto reservation = m_reservations.find(id);
        if (reservation == m_reservations.end())
            return;
        if (reservation->value.released_by_allocator && !reservation->value.claimed && !reservation->value.pinned && reservation->value.claim_window_closed) {
            m_reservations.remove(id);
            m_surfaces.remove(id);
        }
    }

    u64 m_next_canvas_id { 1 };

    HashMap<CanvasId, NonnullRefPtr<Gfx::PaintingSurface>> m_surfaces;
    HashMap<CanvasId, CanvasIdReservation> m_reservations;
};

}
