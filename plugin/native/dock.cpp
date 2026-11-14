// The plugin's Qt dock. It only renders: it reads the view model computed by Rust
// (omr_model_json), sends commands (omr_command) and opens the dialogs Rust
// asks for. It contains no streaming or configuration logic.
#include "pch.h"
#include "edit-targets-dialog.h"
#include "json.hpp"
#include "output-config.h"

#include <QAbstractItemView>
#include <QApplication>
#include <QFrame>
#include <QHBoxLayout>
#include <QMenu>
#include <QPainter>
#include <QPainterPath>
#include <QPixmap>
#include <QPointer>
#include <QSignalBlocker>
#include <QToolButton>
#include <QVBoxLayout>

#include <algorithm>
#include <map>
#include <vector>

extern "C" {
char* omr_model_json();
void omr_command(const char* json);
char* omr_edit_begin();
void omr_edit_apply(const char* json);
void omr_free(char*);
}

namespace {

// Painted icons (not glyphs): the pause emoji renders inside a blue box and
// glyph sizes vary with the font. This way they are crisp and use the theme color.
enum class Glyph { Pause, Play, Restart };
constexpr int kIconPx = 18;

QIcon GlyphIcon(Glyph g, const QColor& color, qreal dpr)
{
    QPixmap pm(QSize(kIconPx, kIconPx) * dpr);
    pm.setDevicePixelRatio(dpr);
    pm.fill(Qt::transparent);
    QPainter p(&pm);
    p.setRenderHint(QPainter::Antialiasing);
    p.scale(kIconPx / 16.0, kIconPx / 16.0); // draw on a 16x16 grid
    switch (g) {
    case Glyph::Pause:
        p.setPen(Qt::NoPen);
        p.setBrush(color);
        p.drawRoundedRect(QRectF(3.5, 3, 3.2, 10), 0.8, 0.8);
        p.drawRoundedRect(QRectF(9.3, 3, 3.2, 10), 0.8, 0.8);
        break;
    case Glyph::Play: {
        p.setPen(Qt::NoPen);
        p.setBrush(color);
        QPainterPath tri;
        tri.moveTo(4.5, 2.5);
        tri.lineTo(13.5, 8);
        tri.lineTo(4.5, 13.5);
        tri.closeSubpath();
        p.drawPath(tri);
        break;
    }
    case Glyph::Restart: {
        // Clockwise arc with a gap at the top left and an arrowhead.
        QPen pen(color, 1.8, Qt::SolidLine, Qt::RoundCap);
        p.setPen(pen);
        p.setBrush(Qt::NoBrush);
        p.drawArc(QRectF(3, 3, 10, 10), 120 * 16, -300 * 16);
        p.setPen(Qt::NoPen);
        p.setBrush(color);
        QPainterPath head;
        head.moveTo(3, 4.2);
        head.lineTo(0.4, 8.6);
        head.lineTo(5.6, 8.6);
        head.closeSubpath();
        p.drawPath(head);
        break;
    }
    }
    return QIcon(pm);
}

// Flat button: no gray box, just a subtle background on hover.
QToolButton* FlatIconButton(QWidget* parent)
{
    auto b = new QToolButton(parent);
    b->setAutoRaise(true);
    b->setToolButtonStyle(Qt::ToolButtonIconOnly);
    b->setIconSize(QSize(kIconPx, kIconPx));
    b->setFixedSize(kIconPx + 4, kIconPx + 4);
    b->setCursor(Qt::PointingHandCursor);
    b->setStyleSheet("QToolButton { border: none; background: transparent; padding: 0px; margin: 0px; }"
                     "QToolButton:hover { background: rgba(127, 127, 127, 0.25); border-radius: 4px; }"
                     "QToolButton:pressed { background: rgba(127, 127, 127, 0.45); border-radius: 4px; }");
    return b;
}

QColor IconColor(const QWidget* w)
{
    return w->palette().color(QPalette::WindowText);
}

} // namespace

using json = nlohmann::json;

namespace {

QString T(const char* key)
{
    return QString::fromUtf8(obs_module_text(key));
}

void Command(const json& cmd)
{
    omr_command(cmd.dump().c_str());
}

json TakeJson(char* raw)
{
    auto parsed = json::parse(raw ? raw : "{}", nullptr, false);
    omr_free(raw);
    return parsed.is_discarded() ? json::object() : parsed;
}

std::string Str(const json& j, const char* key)
{
    auto it = j.find(key);
    return it != j.end() && it->is_string() ? it->get<std::string>() : std::string();
}

bool Bool(const json& j, const char* key)
{
    auto it = j.find(key);
    return it != j.end() && it->is_boolean() && it->get<bool>();
}

// ------------------------------------------------------------------ target row

class TargetRow : public QWidget {
public:
    explicit TargetRow(const std::string& id, QWidget* parent = nullptr) : QWidget(parent), id_(id)
    {
        auto layout = new QGridLayout(this);
        // Compact margins: the style's defaults (~9-11 px per side) spread targets too far apart.
        layout->setContentsMargins(6, 3, 6, 3);
        layout->setHorizontalSpacing(6);
        layout->setVerticalSpacing(2);

        layout->addWidget(enabled_ = new QCheckBox(this), 0, 0);
        enabled_->setToolTip(T("Target.Enabled"));
        QObject::connect(enabled_, &QCheckBox::clicked, [this](bool checked) {
            Command({{"cmd", "set_enabled"}, {"id", id_}, {"enabled", checked}});
        });

        auto title = new QWidget(this);
        auto titleLayout = new QHBoxLayout(title);
        titleLayout->setContentsMargins(0, 0, 0, 0);
        titleLayout->setSpacing(0);
        titleLayout->addWidget(dot_ = new QLabel(QString::fromUtf8("●"), this));
        titleLayout->addWidget(name_ = new QLabel(this));
        // Streaming time next to the name (targets using the Rust engine).
        titleLayout->addSpacing(8);
        titleLayout->addWidget(clock_ = new QLabel(this));
        clock_->hide();
        titleLayout->addStretch(1);
        layout->addWidget(title, 0, 1);

        layout->addWidget(button_ = new QPushButton(T("Btn.Start"), this), 0, 2);
        QObject::connect(button_, &QPushButton::clicked, [this]() { Command({{"cmd", "toggle"}, {"id", id_}}); });
        button_->setSizePolicy(QSizePolicy::Minimum, QSizePolicy::Preferred);
        button_->setMinimumHeight(button_->fontMetrics().height() + 8);
        auto fm = button_->fontMetrics();
        button_->setMinimumWidth((std::max)(fm.horizontalAdvance(T("Btn.Start")), fm.horizontalAdvance(T("Status.Stop"))) + 24);
        layout->setColumnStretch(1, 1);

        layout->addWidget(msg_ = new QLabel(this), 1, 0, 1, 3);
        msg_->setWordWrap(true);
        msg_->setSizePolicy(QSizePolicy::Preferred, QSizePolicy::Minimum);

        BuildRustControls(layout);
    }

    const std::string& Id() const { return id_; }

    void Apply(const json& m)
    {
        name_->setText(QString::fromStdString(Str(m, "name")));
        {
            QSignalBlocker block(enabled_);
            enabled_->setChecked(Bool(m, "enabled"));
        }
        button_->setText(QString::fromStdString(Str(m, "button")));
        auto clock = QString::fromStdString(Str(m, "clock"));
        if (clock_->text() != clock)
            clock_->setText(clock);
        clock_->setVisible(!clock.isEmpty());
        auto msg = QString::fromStdString(Str(m, "msg"));
        if (msg_->text() != msg)
            msg_->setText(msg);
        msg_->setVisible(!msg.isEmpty());
        msg_->setToolTip(QString::fromStdString(Str(m, "tooltip")));
        auto dot = QString("color: %1;").arg(QString::fromStdString(Str(m, "dot")));
        if (dot_->styleSheet() != dot)
            dot_->setStyleSheet(dot);

        const auto& rust = m.contains("rust") ? m["rust"] : json();
        rustControls_->setVisible(rust.is_object());
        if (rust.is_object()) {
            const char* slots[2] = {"primary", "backup"};
            for (int i = 0; i < 2; ++i) {
                const auto& s = rust.contains(slots[i]) ? rust[slots[i]] : json();
                slotBox_[i]->setVisible(s.is_object());
                SetPaused(i, s.is_object() && Bool(s, "paused"));
                if (s.is_object()) {
                    auto rate = QString::fromStdString(Str(s, "bitrate"));
                    if (slotRate_[i]->text() != rate)
                        slotRate_[i]->setText(rate);
                    auto color = QString("color: %1;").arg(QString::fromStdString(Str(s, "dot")));
                    if (slotDot_[i]->styleSheet() != color)
                        slotDot_[i]->setStyleSheet(color);
                    auto info = QString::fromStdString(Str(s, "info"));
                    if (slotInfo_[i]->text() != info)
                        slotInfo_[i]->setText(info);
                    auto tip = QString::fromStdString(Str(s, "tooltip"));
                    slotBox_[i]->setToolTip(tip.isEmpty() ? info : tip);
                }
            }
        }
    }

private:
    std::string id_;
    QCheckBox* enabled_ = nullptr;
    QLabel* dot_ = nullptr;
    QLabel* name_ = nullptr;
    QLabel* clock_ = nullptr;
    QPushButton* button_ = nullptr;
    QLabel* msg_ = nullptr;
    QWidget* rustControls_ = nullptr;
    QWidget* slotBox_[2] = {};
    QToolButton* pauseBtn_[2] = {};
    QLabel* slotDot_[2] = {};
    QLabel* slotRate_[2] = {};
    QLabel* slotInfo_[2] = {};
    bool paused_[2] = {};

    void SetPaused(int i, bool paused)
    {
        paused_[i] = paused;
        pauseBtn_[i]->setIcon(GlyphIcon(paused ? Glyph::Play : Glyph::Pause, IconColor(this), devicePixelRatioF()));
        pauseBtn_[i]->setToolTip(T(paused ? "Rust.Resume" : "Rust.Pause"));
    }

    // One row per stream (Live / Backup) of a target using the Rust engine:
    // ● Live ⏸ ⟳  6.30 Mbps  30 FPS  00:12:03 · recon 1 · drop 3
    void BuildRustControls(QGridLayout* layout)
    {
        rustControls_ = new QWidget(this);
        auto rows = new QVBoxLayout(rustControls_);
        rows->setContentsMargins(0, 0, 0, 0);
        rows->setSpacing(0);
        QLabel* names[2];
        for (int i = 0; i < 2; ++i) {
            const char* slot = i == 0 ? "primary" : "backup";
            auto box = slotBox_[i] = new QWidget(rustControls_);
            auto row = new QHBoxLayout(box);
            row->setContentsMargins(0, 0, 0, 0);
            row->setSpacing(4);

            slotDot_[i] = new QLabel(QString::fromUtf8("●"), box);
            slotDot_[i]->setStyleSheet("color: #888888;");
            row->addWidget(slotDot_[i]);
            auto label = T(i == 0 ? "Rust.Primary" : "Rust.Backup");
            if (label.endsWith(':'))
                label.chop(1);
            names[i] = new QLabel(label, box);
            row->addWidget(names[i]);

            pauseBtn_[i] = FlatIconButton(box);
            SetPaused(i, false);
            QObject::connect(pauseBtn_[i], &QToolButton::clicked, [this, i, slot]() {
                Command({{"cmd", "rust"}, {"id", id_}, {"slot", slot}, {"action", paused_[i] ? "resume" : "pause"}});
            });
            auto restart = FlatIconButton(box);
            restart->setIcon(GlyphIcon(Glyph::Restart, IconColor(this), devicePixelRatioF()));
            restart->setToolTip(T("Rust.Restart"));
            QObject::connect(restart, &QToolButton::clicked, [this, slot]() {
                Command({{"cmd", "rust"}, {"id", id_}, {"slot", slot}, {"action", "restart"}});
            });
            row->addWidget(pauseBtn_[i]);
            row->addWidget(restart);

            // Fixed width so both rows line up in columns.
            slotRate_[i] = new QLabel(box);
            slotRate_[i]->setMinimumWidth(slotRate_[i]->fontMetrics().horizontalAdvance("00.00 Mbps  60 FPS"));
            slotRate_[i]->setAlignment(Qt::AlignRight | Qt::AlignVCenter);
            row->addWidget(slotRate_[i]);

            // The connection text (which may include the error) must not widen the dock.
            slotInfo_[i] = new QLabel(box);
            slotInfo_[i]->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Preferred);
            row->addWidget(slotInfo_[i], 1);
            rows->addWidget(box);
        }
        // Same width for "Live" and "Backup" (and their translations).
        int w = std::max(names[0]->sizeHint().width(), names[1]->sizeHint().width());
        names[0]->setMinimumWidth(w);
        names[1]->setMinimumWidth(w);
        layout->addWidget(rustControls_, 2, 0, 1, 3);
        rustControls_->hide();
    }
};

// ------------------------------------------------------------------ list that grows with its content

class OutputsList : public QListWidget {
public:
    using QListWidget::QListWidget;

    QSize sizeHint() const override
    {
        QSize hint = QListWidget::sizeHint();
        hint.setHeight(ContentHeight());
        return hint;
    }
    QSize minimumSizeHint() const override { return sizeHint(); }

protected:
    // Each item's widgets change height (multi-line messages,
    // Rust controls): propagate their sizeHint to the item that holds them.
    bool eventFilter(QObject* obj, QEvent* event) override
    {
        if (event->type() == QEvent::LayoutRequest && RefreshItemSizeHints())
            updateGeometry();
        return QListWidget::eventFilter(obj, event);
    }

    bool event(QEvent* event) override
    {
        const bool handled = QListWidget::event(event);
        switch (event->type()) {
        case QEvent::FontChange:
        case QEvent::LayoutRequest:
        case QEvent::PolishRequest:
        case QEvent::Show:
        case QEvent::StyleChange:
            if (RefreshItemSizeHints())
                updateGeometry();
            break;
        default:
            break;
        }
        return handled;
    }

private:
    bool RefreshItemSizeHints()
    {
        bool changed = false;
        for (int i = 0; i < count(); ++i) {
            auto it = item(i);
            auto w = it ? itemWidget(it) : nullptr;
            if (!w)
                continue;
            auto hint = w->sizeHint();
            if (it->sizeHint() != hint) {
                it->setSizeHint(hint);
                changed = true;
            }
        }
        return changed;
    }

    int ContentHeight() const
    {
        auto* self = const_cast<OutputsList*>(this);
        self->doItemsLayout();
        int total = frameWidth() * 2;
        for (int i = 0; i < count(); ++i)
            total += sizeHintForRow(i);
        if (count() > 1)
            total += (count() - 1) * spacing();
        return (std::max)(total, frameWidth() * 2);
    }
};

// ------------------------------------------------------------------ dock

class Dock : public QWidget {
public:
    Dock()
    {
        setWindowTitle(T("Title"));
        container_ = new QWidget(&scroll_);
        auto layout = new QVBoxLayout(container_);
        layout->setAlignment(Qt::AlignTop);
        layout->setSizeConstraint(QLayout::SetMinAndMaxSize);
        // No style margin (~10 px) on top: the list starts right below the header.
        layout->setContentsMargins(0, 0, 0, 4);
        layout->setSpacing(2);

        // Fixed header (outside the scroll area): a compact bar that neither scrolls
        // with the list nor moves when targets are added.
        auto header = new QWidget(this);
        auto headerLayout = new QVBoxLayout(header);
        headerLayout->setContentsMargins(6, 4, 6, 2);
        headerLayout->setSpacing(2);
        auto bar = new QHBoxLayout();
        bar->setContentsMargins(0, 0, 0, 0);
        bar->setSpacing(4);
        auto tool = [header](const char* glyph, const char* key) {
            auto b = new QToolButton(header);
            b->setText(QString::fromUtf8(glyph) + " " + T(key));
            b->setToolTip(T(key));
            b->setToolButtonStyle(Qt::ToolButtonTextOnly);
            b->setSizePolicy(QSizePolicy::Fixed, QSizePolicy::Fixed);
            return b;
        };
        auto startAll = tool("▶", "Btn.StartAll");
        auto stopAll = tool("■", "Btn.StopAll");
        auto edit = tool("✎", "Btn.EditTargets");
        QObject::connect(startAll, &QToolButton::clicked, []() { Command({{"cmd", "start_all"}}); });
        QObject::connect(stopAll, &QToolButton::clicked, []() { Command({{"cmd", "stop_all"}}); });
        QObject::connect(edit, &QToolButton::clicked, [this]() { EditTargets(); });
        bar->addWidget(startAll);
        bar->addWidget(stopAll);
        bar->addStretch(1);
        bar->addWidget(edit);

        // Advanced options menu (next to Edit targets).
        advanced_ = new QToolButton(header);
        advanced_->setText(QString::fromUtf8("⚙"));
        advanced_->setToolTip(T("Btn.Advanced"));
        advanced_->setPopupMode(QToolButton::InstantPopup);
        advanced_->setSizePolicy(QSizePolicy::Fixed, QSizePolicy::Fixed);
        auto menu = new QMenu(advanced_);
        menu->setToolTipsVisible(true);
        nullStream_ = menu->addAction(T("MainStreamNull"));
        nullStream_->setToolTip(T("MainStreamNull.Tooltip"));
        nullStream_->setCheckable(true);
        nullStream_->setEnabled(false); // until FINISHED_LOADING
        QObject::connect(nullStream_, &QAction::triggered,
                         [](bool checked) { Command({{"cmd", "null_stream"}, {"enabled", checked}}); });
        congestionReconnect_ = menu->addAction(T("CongestionReconnect"));
        congestionReconnect_->setToolTip(T("CongestionReconnect.Tooltip"));
        congestionReconnect_->setCheckable(true);
        congestionReconnect_->setEnabled(false); // until FINISHED_LOADING
        QObject::connect(congestionReconnect_, &QAction::triggered,
                         [](bool checked) { Command({{"cmd", "congestion_reconnect"}, {"enabled", checked}}); });
        advanced_->setMenu(menu);
        bar->addWidget(advanced_);
        headerLayout->addLayout(bar);

        auto line = new QFrame(header);
        line->setFrameShape(QFrame::HLine);
        line->setFrameShadow(QFrame::Sunken);
        headerLayout->addWidget(line);

        list_ = new OutputsList(container_);
        list_->setDragDropMode(QAbstractItemView::InternalMove);
        list_->setSelectionMode(QAbstractItemView::SingleSelection);
        list_->setDropIndicatorShown(true);
        list_->setSizePolicy(QSizePolicy::Expanding, QSizePolicy::Maximum);
        list_->setSizeAdjustPolicy(QAbstractScrollArea::AdjustToContents);
        list_->setVerticalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
        list_->setHorizontalScrollBarPolicy(Qt::ScrollBarAsNeeded);
        list_->setStyleSheet("QListWidget { padding: 0px; margin: 0px; border: none; background: transparent; }"
                             "QListWidget::item:selected, QListWidget::item:active { border: none; background: transparent; }"
                             "QListWidget::item:hover, QListWidget::item:hover:selected, QListWidget::item:hover:active {"
                             " border: none; background: rgba(127, 127, 127, 0.1); }");
        QObject::connect(list_->model(), &QAbstractItemModel::rowsMoved, [this]() { OnRowsMoved(); });
        layout->addWidget(list_);

        auto credit = new QLabel(
            QString::fromUtf8("<p>This plugin is provided for free. <br>Author: SoraYuki "
                              "(<a href=\"https://paypal.me/sorayuki0\">donate</a>)</p>"),
            container_);
        credit->setTextFormat(Qt::RichText);
        credit->setTextInteractionFlags(Qt::TextBrowserInteraction);
        credit->setOpenExternalLinks(true);
        credit->setContentsMargins(6, 2, 6, 0); // aligned with the targets
        layout->addWidget(credit);

        scroll_.setWidgetResizable(true);
        scroll_.setFrameShape(QFrame::NoFrame);
        scroll_.setWidget(container_);
        auto full = new QGridLayout(this);
        full->setContentsMargins(0, 0, 0, 0);
        full->setSpacing(0);
        full->addWidget(header, 0, 0);
        full->addWidget(&scroll_, 1, 0);
        full->setRowStretch(1, 1);

        timer_.setInterval(1000);
        QObject::connect(&timer_, &QTimer::timeout, []() { Command({{"cmd", "tick"}}); });
        timer_.start();

        Refresh();
    }

    // Coalesces requests: a single repaint per event loop iteration.
    void ScheduleRefresh()
    {
        if (refreshPending_)
            return;
        refreshPending_ = true;
        QTimer::singleShot(0, this, [this]() {
            refreshPending_ = false;
            Refresh();
        });
    }

    void Refresh()
    {
        json m = TakeJson(omr_model_json());
        if (!m.is_object() || !m.contains("targets"))
            return;

        const auto& ns = m["null_stream"];
        const bool nullActive = Bool(ns, "checked");
        {
            QSignalBlocker block(nullStream_);
            nullStream_->setChecked(nullActive);
        }
        nullStream_->setEnabled(Bool(ns, "enabled"));
        const auto& cr = m.contains("congestion_reconnect") ? m["congestion_reconnect"] : json();
        {
            QSignalBlocker block(congestionReconnect_);
            congestionReconnect_->setChecked(Bool(cr, "checked"));
        }
        congestionReconnect_->setEnabled(Bool(cr, "enabled"));
        // When the null output is active the button shows it, since the option is hidden in the menu.
        auto advText = QString::fromUtf8(nullActive ? "⚙ ⊘" : "⚙");
        if (advanced_->text() != advText) {
            advanced_->setText(advText);
            advanced_->setStyleSheet(nullActive ? "QToolButton { color: #e6b800; }" : "");
            advanced_->setToolTip(nullActive ? T("Btn.Advanced") + QStringLiteral("\n") + T("MainStreamNull") : T("Btn.Advanced"));
        }

        std::vector<std::string> ids;
        for (const auto& t : m["targets"])
            ids.push_back(Str(t, "id"));
        if (ids != CurrentIds())
            RebuildRows(ids);
        int i = 0;
        for (const auto& t : m["targets"]) {
            if (auto row = RowAt(i++))
                row->Apply(t);
        }
    }

protected:
    void showEvent(QShowEvent* e) override
    {
        ScheduleRefresh();
        QWidget::showEvent(e);
    }

private:
    QScrollArea scroll_;
    QWidget* container_ = nullptr;
    QToolButton* advanced_ = nullptr;
    QAction* nullStream_ = nullptr;
    QAction* congestionReconnect_ = nullptr;
    OutputsList* list_ = nullptr;
    QTimer timer_;
    bool refreshPending_ = false;

    TargetRow* RowAt(int i) const
    {
        auto it = list_->item(i);
        return it ? dynamic_cast<TargetRow*>(list_->itemWidget(it)) : nullptr;
    }

    std::vector<std::string> CurrentIds() const
    {
        std::vector<std::string> ids;
        for (int i = 0; i < list_->count(); ++i) {
            auto it = list_->item(i);
            ids.push_back(it ? it->data(Qt::UserRole).toString().toStdString() : std::string());
        }
        return ids;
    }

    void RebuildRows(const std::vector<std::string>& ids)
    {
        list_->clear();
        for (const auto& id : ids) {
            auto row = new TargetRow(id, list_->viewport());
            auto item = new QListWidgetItem();
            item->setData(Qt::UserRole, QString::fromStdString(id));
            item->setSizeHint(row->sizeHint());
            list_->addItem(item);
            list_->setItemWidget(item, row);
            row->installEventFilter(list_);
        }
        list_->updateGeometry();
    }

    void OnRowsMoved()
    {
        json ids = json::array();
        for (const auto& id : CurrentIds())
            ids.push_back(id);
        list_->clearSelection();
        Command({{"cmd", "reorder"}, {"ids", ids}});
    }

    void EditTargets()
    {
        json begin = TakeJson(omr_edit_begin());
        if (!begin.contains("config"))
            return;
        GlobalEditSnapshot snapshot;
        for (const auto& id : begin["active_targets"])
            snapshot.activeTargetIds.insert(id.get<std::string>());
        for (const auto& id : begin["active_video"])
            snapshot.activeVideoConfigIds.insert(id.get<std::string>());
        for (const auto& id : begin["active_audio"])
            snapshot.activeAudioConfigIds.insert(id.get<std::string>());

        auto result = showEditTargetsDialog(Str(begin, "config"), snapshot, this);
        if (result.has_value())
            omr_edit_apply(result->c_str());
    }
};

QPointer<Dock> g_dock;

} // namespace

// ------------------------------------------------------------------ bridge for Rust (src/ui.rs)

extern "C" bool omr_ui_create_dock()
{
    auto dock = new Dock();
    dock->setObjectName("obs-multi-rtmp-dock");
    if (!obs_frontend_add_dock_by_id("obs-multi-rtmp-dock", obs_module_text("Title"), dock)) {
        delete dock;
        return false;
    }
    g_dock = dock;
    return true;
}

extern "C" void omr_ui_post(void (*func)(void*), void* data)
{
    // Always queued on the UI thread (qApp lives on the main thread).
    QMetaObject::invokeMethod(qApp, [func, data]() { func(data); }, Qt::QueuedConnection);
}

extern "C" void omr_ui_refresh()
{
    QMetaObject::invokeMethod(qApp, []() {
        if (g_dock)
            g_dock->ScheduleRefresh();
    }, Qt::QueuedConnection);
}

extern "C" bool omr_ui_question(const char* title, const char* text)
{
    auto parent = g_dock ? static_cast<QWidget*>(g_dock.data()) : nullptr;
    auto answer = QMessageBox::question(parent, QString::fromUtf8(title), QString::fromUtf8(text),
                                        QMessageBox::Yes | QMessageBox::No, QMessageBox::No);
    return answer == QMessageBox::Yes;
}

extern "C" void omr_ui_message(int icon, const char* title, const char* text)
{
    auto parent = g_dock ? static_cast<QWidget*>(g_dock.data()) : nullptr;
    auto qicon = icon == 2 ? QMessageBox::Critical : icon == 1 ? QMessageBox::Warning : QMessageBox::Information;
    QMessageBox(qicon, QString::fromUtf8(title), QString::fromUtf8(text), QMessageBox::Ok, parent).exec();
}
