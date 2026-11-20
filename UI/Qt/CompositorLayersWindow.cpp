/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <UI/Qt/CompositorLayersWindow.h>
#include <UI/Qt/StringUtils.h>

#include <QHeaderView>
#include <QLabel>
#include <QPixmap>
#include <QSignalBlocker>
#include <QTimer>
#include <QTreeWidget>
#include <QVBoxLayout>

namespace Ladybird {

namespace {

enum Column {
    Layer,
    Holds,
};

constexpr int LayerIDRole = Qt::UserRole + 1;
constexpr int REFRESH_INTERVAL_IN_MILLISECONDS = 150;
constexpr int SWATCH_SIZE = 12;

QIcon swatch_of(u32 rgb)
{
    QPixmap pixmap(SWATCH_SIZE, SWATCH_SIZE);
    pixmap.fill(QColor::fromRgb(rgb));
    return QIcon(pixmap);
}

}

CompositorLayersWindow::CompositorLayersWindow(Source source)
    : QDialog(nullptr, Qt::Tool)
    , m_source(AK::move(source))
{
    setWindowTitle("Compositor Layers");
    resize(560, 420);

    m_layer_tree = new QTreeWidget(this);
    m_layer_tree->setColumnCount(2);
    m_layer_tree->setHeaderLabels({ "Layer", "Holds" });
    m_layer_tree->setRootIsDecorated(false);
    m_layer_tree->setUniformRowHeights(true);
    m_layer_tree->setSelectionMode(QAbstractItemView::NoSelection);
    m_layer_tree->header()->setSectionResizeMode(Layer, QHeaderView::Stretch);
    m_layer_tree->header()->setSectionResizeMode(Holds, QHeaderView::ResizeToContents);
    m_layer_tree->header()->setStretchLastSection(false);

    // The notes and the summary show text from other processes. Do not read that text as markup.
    m_notes = new QLabel(this);
    m_notes->setTextFormat(Qt::PlainText);
    m_notes->setWordWrap(true);
    m_summary_of_last_frame = new QLabel(this);
    m_summary_of_last_frame->setTextFormat(Qt::PlainText);
    m_summary_of_last_frame->setWordWrap(true);
    m_tile_highlight_legend = new QLabel(this);
    m_tile_highlight_legend->setTextFormat(Qt::RichText);
    m_tile_highlight_legend->setWordWrap(true);

    auto* layout = new QVBoxLayout(this);
    layout->addWidget(m_layer_tree, 1);
    layout->addWidget(m_notes);
    layout->addWidget(m_summary_of_last_frame);
    layout->addWidget(m_tile_highlight_legend);

    QObject::connect(m_layer_tree, &QTreeWidget::itemChanged, this, [this](QTreeWidgetItem* item, int column) {
        if (column != Layer)
            return;
        auto layer_id = item->data(Layer, LayerIDRole).toULongLong();
        m_source.set_layer_hidden_in_active_tab(layer_id, item->checkState(Layer) == Qt::Unchecked);
    });

    m_refresh_timer = new QTimer(this);
    m_refresh_timer->setInterval(REFRESH_INTERVAL_IN_MILLISECONDS);
    QObject::connect(m_refresh_timer, &QTimer::timeout, this, [this] { refresh(); });
}

void CompositorLayersWindow::showEvent(QShowEvent* event)
{
    QDialog::showEvent(event);
    refresh();
    m_refresh_timer->start();
}

void CompositorLayersWindow::hideEvent(QHideEvent* event)
{
    m_refresh_timer->stop();
    QDialog::hideEvent(event);
}

// All ways to close the dialog call done(): the Escape key and the close button of the window call reject(), which
// calls done().
void CompositorLayersWindow::done(int result)
{
    QDialog::done(result);
    if (on_close)
        on_close();
}

void CompositorLayersWindow::refresh()
{
    auto description = m_source.layers_of_active_tab();
    if (description == m_shown_description)
        return;

    QSignalBlocker block_hide_requests_while_showing_what_compositor_hides(m_layer_tree);

    // Keeping the rows of the same layers keeps the list where it was scrolled to.
    auto rows_are_of_same_layers = description.has_value() && m_shown_description.has_value() && description->layers.size() == m_shown_description->layers.size();
    for (size_t i = 0; rows_are_of_same_layers && i < description->layers.size(); ++i)
        rows_are_of_same_layers = description->layers[i].id == m_shown_description->layers[i].id;
    if (!rows_are_of_same_layers) {
        m_layer_tree->clear();
        for (size_t i = 0; description.has_value() && i < description->layers.size(); ++i) {
            auto* item = new QTreeWidgetItem(m_layer_tree);
            item->setFlags(Qt::ItemIsEnabled | Qt::ItemIsUserCheckable);
        }
    }

    if (!description.has_value()) {
        m_notes->clear();
        m_summary_of_last_frame->setText("The active tab has presented no frame since the layers were shown.");
        m_tile_highlight_legend->clear();
        m_shown_description = AK::move(description);
        return;
    }

    auto const& layers_from_base_up = description->layers;
    auto layer_count = layers_from_base_up.size();
    for (size_t row = 0; row < layer_count; ++row) {
        auto const& layer = layers_from_base_up[layer_count - 1 - row];
        auto* item = m_layer_tree->topLevelItem(static_cast<int>(row));
        item->setData(Layer, LayerIDRole, QVariant::fromValue<qulonglong>(layer.id));
        item->setCheckState(Layer, layer.hidden ? Qt::Unchecked : Qt::Checked);
        item->setIcon(Layer, swatch_of(layer.rgb_of_outline));
        item->setText(Layer, qstring_from_ak_string(layer.name));
        item->setText(Holds, qstring_from_ak_string(layer.details));
    }

    QStringList notes;
    for (auto const& note : description->notes)
        notes.append(qstring_from_ak_string(note));
    m_notes->setText(notes.join('\n'));
    m_notes->setVisible(!notes.isEmpty());
    m_summary_of_last_frame->setText(qstring_from_ak_string(description->summary_of_last_frame));

    QStringList tile_highlight_legend_entries;
    for (auto const& highlight : description->tile_highlights) {
        auto color = QColor::fromRgb(highlight.rgb).name();
        tile_highlight_legend_entries.append(QString("<span style=\"color: %1\">&#9632;</span> %2").arg(color, qstring_from_ak_string(highlight.raster_cause).toHtmlEscaped()));
    }
    m_tile_highlight_legend->setText("Tiles rastered are highlighted by why: " + tile_highlight_legend_entries.join("&nbsp;&nbsp; "));

    m_shown_description = AK::move(description);
}

}
