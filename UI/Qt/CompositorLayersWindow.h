/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Function.h>
#include <AK/Optional.h>
#include <AK/kmalloc.h>
#include <LibCompositing/Types.h>

#include <QDialog>

class QLabel;
class QTimer;
class QTreeWidget;

namespace Ladybird {

class CompositorLayersWindow final : public QDialog {
public:
    AK_ALLOC_WITH_KMALLOC;

    struct Source {
        Function<Optional<Compositing::CompositorLayersDescription>()> layers_of_active_tab;
        Function<void(u64 layer_id, bool hidden)> set_layer_hidden_in_active_tab;
    };

    explicit CompositorLayersWindow(Source);

    Function<void()> on_close;

private:
    virtual void showEvent(QShowEvent*) override;
    virtual void hideEvent(QHideEvent*) override;
    virtual void done(int result) override;
    void refresh();

    Source m_source;
    Optional<Compositing::CompositorLayersDescription> m_shown_description;
    QTreeWidget* m_layer_tree { nullptr };
    QLabel* m_notes { nullptr };
    QLabel* m_summary_of_last_frame { nullptr };
    QLabel* m_tile_highlight_legend { nullptr };
    QTimer* m_refresh_timer { nullptr };
};

}
