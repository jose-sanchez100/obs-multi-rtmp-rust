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
enum class Glyph { Pause, Play, Restart, Stop, Edit, Options };
constexpr int kIconPx = 18;

QIcon GlyphIcon(Glyph g, const QColor& color, qreal dpr, int px = kIconPx)
{
    QPixmap pm(QSize(px, px) * dpr);
    pm.setDevicePixelRatio(dpr);
    pm.fill(Qt::transparent);
    QPainter p(&pm);
    p.setRenderHint(QPainter::Antialiasing);
    p.scale(px / 16.0, px / 16.0); // draw on a 16x16 grid
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
    case Glyph::Stop:
        p.setPen(Qt::NoPen);
        p.setBrush(color);
        p.drawRoundedRect(QRectF(3, 3, 10, 10), 1.5, 1.5);
        break;
    case Glyph::Edit: {
        // Pencil outline.
        p.setPen(QPen(color, 1.5, Qt::SolidLine, Qt::RoundCap, Qt::RoundJoin));
        p.setBrush(Qt::NoBrush);
        QPainterPath pencil;
        pencil.moveTo(10.5, 2.5);
        pencil.lineTo(13.5, 5.5);
        pencil.lineTo(5.5, 13.5);
        pencil.lineTo(2.5, 13.5);
        pencil.lineTo(2.5, 10.5);
        pencil.closeSubpath();
        p.drawPath(pencil);
        break;
    }
    case Glyph::Options:
        // Two sliders.
        p.setPen(QPen(color, 1.5, Qt::SolidLine, Qt::RoundCap));
        p.setBrush(Qt::NoBrush);
        p.drawLine(QPointF(2, 4), QPointF(8.9, 4));
        p.drawLine(QPointF(12.1, 4), QPointF(14, 4));
        p.drawLine(QPointF(2, 12), QPointF(3.9, 12));
        p.drawLine(QPointF(7.1, 12), QPointF(14, 12));
        p.drawEllipse(QPointF(10.5, 4), 1.6, 1.6);
        p.drawEllipse(QPointF(5.5, 12), 1.6, 1.6);
        break;
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

// ------------------------------------------------------------------ target card

// Colors that follow the OBS theme: dark and light variants of each status tone.
enum class Tone { Live, Warn, Error, Busy };

bool IsDark(const QWidget* w)
{
    return w->palette().color(QPalette::Window).lightness() < 128;
}

QColor ToneFg(Tone t, bool dark)
{
    switch (t) {
    case Tone::Live:
        return QColor(dark ? "#6ee7a2" : "#11663a");
    case Tone::Warn:
        return QColor(dark ? "#ffd659" : "#7a5800");
    case Tone::Error:
        return QColor(dark ? "#ff8f86" : "#a3221b");
    case Tone::Busy:
        return QColor(dark ? "#9cc0ff" : "#1d4ed8");
    }
    return {};
}

QColor ToneBg(Tone t, bool dark)
{
    switch (t) {
    case Tone::Live:
        return QColor(dark ? "#173d29" : "#d3f5e1");
    case Tone::Warn:
        return QColor(dark ? "#4a3d12" : "#fff0c2");
    case Tone::Error:
        return QColor(dark ? "#4d1f1c" : "#fde0dd");
    case Tone::Busy:
        return QColor(dark ? "#1d2f57" : "#dbe6ff");
    }
    return {};
}

QString Rgba(const QColor& c, double alpha)
{
    return QString("rgba(%1, %2, %3, %4)").arg(c.red()).arg(c.green()).arg(c.blue()).arg(alpha);
}

QFont ScaledFont(QFont f, qreal factor, bool bold = false)
{
    if (f.pointSizeF() > 0)
        f.setPointSizeF(f.pointSizeF() * factor);
    else if (f.pixelSize() > 0)
        f.setPixelSize(qRound(f.pixelSize() * factor));
    f.setBold(bold);
    return f;
}

void SetText(QLabel* label, const QString& text)
{
    if (label->text() != text)
        label->setText(text);
}

void SetStyle(QWidget* w, const QString& style)
{
    if (w->styleSheet() != style)
        w->setStyleSheet(style);
}

// Signal strength of a stream: four bars, `level` of them lit.
class SignalBars : public QWidget {
public:
    explicit SignalBars(QWidget* parent) : QWidget(parent) { setFixedSize(16, 14); }

    void Set(int level, const QColor& color)
    {
        if (level == level_ && color == color_)
            return;
        level_ = level;
        color_ = color;
        update();
    }

protected:
    void paintEvent(QPaintEvent*) override
    {
        QPainter p(this);
        p.setRenderHint(QPainter::Antialiasing);
        p.setPen(Qt::NoPen);
        QColor off = palette().color(QPalette::WindowText);
        off.setAlphaF(0.22);
        for (int i = 0; i < 4; ++i) {
            const qreal h = 5 + i * 3;
            p.setBrush(i < level_ ? color_ : off);
            p.drawRoundedRect(QRectF(i * 4, height() - h, 3, h), 1, 1);
        }
    }

private:
    int level_ = 0;
    QColor color_;
};

// Checkbox drawn the same way in every theme: a white box when off, blue with a
// white tick when on (the theme's indicator is hard to see on the card).
class CardCheck : public QCheckBox {
public:
    explicit CardCheck(QWidget* parent) : QCheckBox(parent)
    {
        setCursor(Qt::PointingHandCursor);
        setFixedSize(kSize + 2, kSize + 2);
    }

    QSize sizeHint() const override { return {kSize + 2, kSize + 2}; }
    QSize minimumSizeHint() const override { return sizeHint(); }

protected:
    bool hitButton(const QPoint& pos) const override { return rect().contains(pos); }

    void paintEvent(QPaintEvent*) override
    {
        QPainter p(this);
        p.setRenderHint(QPainter::Antialiasing);
        const QRectF box(1.5, 1.5, kSize - 1, kSize - 1);
        const bool hover = underMouse();
        if (isChecked()) {
            const QColor blue(hover ? "#6a9dff" : "#4f8cff");
            p.setPen(QPen(blue, 1));
            p.setBrush(blue);
            p.drawRoundedRect(box, 3, 3);
            p.setPen(QPen(Qt::white, 2, Qt::SolidLine, Qt::RoundCap, Qt::RoundJoin));
            p.setBrush(Qt::NoBrush);
            QPainterPath tick;
            tick.moveTo(box.left() + box.width() * 0.24, box.top() + box.height() * 0.52);
            tick.lineTo(box.left() + box.width() * 0.43, box.top() + box.height() * 0.71);
            tick.lineTo(box.left() + box.width() * 0.77, box.top() + box.height() * 0.32);
            p.drawPath(tick);
        } else {
            p.setPen(QPen(QColor(hover ? "#4f8cff" : "#8a8f9c"), 1));
            p.setBrush(Qt::white);
            p.drawRoundedRect(box, 3, 3);
        }
    }

private:
    static constexpr int kSize = 15;
};

// One target as a card:
//   [x] YouTube  [WARNING]                     [Stop]
//       01:35:11 · Backup unstable
//   ┌ ● Live    10.2 Mbps  ▂▄▆█        ⏸ ⟳ ┐
//   │ ● Backup  8.14 Mbps  ▂▄          ⏸ ⟳ │
//   └   retx 6.2% · RTT 106 ms · recon 3    ┘
class TargetRow : public QWidget {
public:
    explicit TargetRow(const std::string& id, QWidget* parent = nullptr) : QWidget(parent), id_(id)
    {
        auto outer = new QVBoxLayout(this);
        outer->setContentsMargins(8, 4, 8, 4);
        card_ = new QFrame(this);
        card_->setObjectName("omrCard");
        outer->addWidget(card_);

        auto layout = new QVBoxLayout(card_);
        layout->setContentsMargins(10, 8, 10, 8);
        layout->setSpacing(6);

        auto header = new QHBoxLayout();
        header->setContentsMargins(0, 0, 0, 0);
        header->setSpacing(8);
        header->addWidget(enabled_ = new CardCheck(card_), 0, Qt::AlignVCenter);
        enabled_->setToolTip(T("Target.Enabled"));
        QObject::connect(enabled_, &QCheckBox::clicked, [this](bool checked) {
            Command({{"cmd", "set_enabled"}, {"id", id_}, {"enabled", checked}});
        });

        auto titles = new QVBoxLayout();
        titles->setContentsMargins(0, 0, 0, 0);
        titles->setSpacing(1);
        auto nameRow = new QHBoxLayout();
        nameRow->setContentsMargins(0, 0, 0, 0);
        nameRow->setSpacing(8);
        nameRow->addWidget(name_ = new QLabel(card_));
        name_->setFont(ScaledFont(font(), 1.08, true));
        nameRow->addWidget(badge_ = new QLabel(card_));
        badge_->setFont(ScaledFont(font(), 0.8, true));
        badge_->hide();
        nameRow->addStretch(1);
        titles->addLayout(nameRow);
        titles->addWidget(summary_ = new QLabel(card_));
        summary_->setFont(ScaledFont(font(), 0.92));
        summary_->setWordWrap(true);
        // Long texts wrap instead of widening the dock.
        summary_->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Preferred);
        titles->addWidget(detail_ = new QLabel(card_));
        detail_->setFont(ScaledFont(font(), 0.92));
        detail_->setWordWrap(true);
        detail_->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Preferred);
        detail_->hide();
        header->addLayout(titles, 1);

        header->addWidget(button_ = new QPushButton(T("Btn.Start"), card_), 0, Qt::AlignVCenter);
        QObject::connect(button_, &QPushButton::clicked, [this]() { Command({{"cmd", "toggle"}, {"id", id_}}); });
        button_->setCursor(Qt::PointingHandCursor);
        button_->setFont(ScaledFont(font(), 1.0, true));
        auto fm = QFontMetrics(button_->font());
        button_->setMinimumWidth((std::max)(fm.horizontalAdvance(T("Btn.Start")), fm.horizontalAdvance(T("Status.Stop"))) + 30);
        button_->setMinimumHeight(fm.height() + 12);
        layout->addLayout(header);

        BuildStreams(layout);
    }

    const std::string& Id() const { return id_; }

    void Apply(const json& m)
    {
        const bool dark = IsDark(this);
        const QColor text = palette().color(QPalette::WindowText);
        const QString muted = Rgba(text, 0.68);

        SetText(name_, QString::fromStdString(Str(m, "name")));
        {
            QSignalBlocker block(enabled_);
            enabled_->setChecked(Bool(m, "enabled"));
        }

        const auto badge = Str(m, "badge");
        const auto badgeText = QString::fromStdString(Str(m, "badge_text"));
        badge_->setVisible(!badgeText.isEmpty());
        if (!badgeText.isEmpty()) {
            const Tone tone = badge == "live" ? Tone::Live : badge == "warn" ? Tone::Warn : badge == "error" ? Tone::Error : Tone::Busy;
            SetText(badge_, badgeText);
            SetStyle(badge_, QString("QLabel { color: %1; background: %2; border-radius: 4px; padding: 1px 6px; }")
                                 .arg(ToneFg(tone, dark).name(), ToneBg(tone, dark).name()));
        }

        const auto summary = QString::fromStdString(Str(m, "summary"));
        SetText(summary_, summary);
        summary_->setVisible(!summary.isEmpty());
        SetStyle(summary_, QString("QLabel { color: %1; }").arg(muted));
        summary_->setToolTip(QString::fromStdString(Str(m, "tooltip")));

        const QColor errorFg = ToneFg(Tone::Error, dark);
        const auto detail = QString::fromStdString(Str(m, "detail"));
        SetText(detail_, detail);
        detail_->setVisible(!detail.isEmpty());
        SetStyle(detail_, QString("QLabel { color: %1; }").arg(errorFg.name()));

        SetStyle(card_, QString("QFrame#omrCard { background: rgba(127, 127, 127, 0.10); border: 1px solid %1; border-radius: 8px; }")
                            .arg(badge == "error" ? Rgba(errorFg, 0.45) : QStringLiteral("rgba(127, 127, 127, 0.22)")));
        SetStyle(streams_, QString("QFrame#omrStreams { background: %1; border: none; border-radius: 6px; }")
                               .arg(dark ? "rgba(0, 0, 0, 0.18)" : "rgba(0, 0, 0, 0.05)"));

        button_->setText(QString::fromStdString(Str(m, "button")));
        if (Bool(m, "busy"))
            SetStyle(button_, QString("QPushButton { background: transparent; color: %1; border: 1px solid %2; border-radius: 6px; padding: 4px 13px; }"
                                      "QPushButton:hover { background: %3; }"
                                      "QPushButton:pressed { background: %4; }")
                                  .arg(errorFg.name(), Rgba(errorFg, 0.55), Rgba(errorFg, 0.12), Rgba(errorFg, 0.22)));
        else
            SetStyle(button_, "QPushButton { background: #2f5bd3; color: #ffffff; border: none; border-radius: 6px; padding: 5px 14px; }"
                              "QPushButton:hover { background: #3a68e0; }"
                              "QPushButton:pressed { background: #2850bd; }");

        const auto& rust = m.contains("rust") ? m["rust"] : json();
        streams_->setVisible(rust.is_object());
        if (!rust.is_object())
            return;
        const QColor warnFg = ToneFg(Tone::Warn, dark);
        const char* slots[2] = {"primary", "backup"};
        for (int i = 0; i < 2; ++i) {
            const auto& s = rust.contains(slots[i]) ? rust[slots[i]] : json();
            slotBox_[i]->setVisible(s.is_object());
            SetPaused(i, s.is_object() && Bool(s, "paused"));
            if (!s.is_object())
                continue;
            SetText(slotRate_[i], QString::fromStdString(Str(s, "rate")));
            SetStyle(slotDot_[i], QString("QLabel { background: %1; border-radius: 4px; }").arg(QString::fromStdString(Str(s, "dot"))));
            const int health = s.contains("health") && s["health"].is_number_integer() ? s["health"].get<int>() : 0;
            slotBars_[i]->Set(health, QColor(health >= 3 ? "#2ecc71" : health == 2 ? "#e6b800" : "#e74c3c"));
            const auto issue = QString::fromStdString(Str(s, "issue"));
            SetText(slotIssue_[i], issue);
            slotIssue_[i]->setVisible(!issue.isEmpty());
            SetStyle(slotIssue_[i], QString("QLabel { color: %1; }").arg(Bool(s, "warn") ? warnFg.name() : muted));
            slotBox_[i]->setToolTip(QString::fromStdString(Str(s, "tooltip")));
        }
        SetStyle(separator_, QString("QFrame { background: %1; border: none; }").arg(Rgba(text, 0.12)));
        separator_->setVisible(slotBox_[0]->isVisibleTo(streams_) && slotBox_[1]->isVisibleTo(streams_));
    }

private:
    std::string id_;
    QFrame* card_ = nullptr;
    QCheckBox* enabled_ = nullptr;
    QLabel* name_ = nullptr;
    QLabel* badge_ = nullptr;
    QLabel* summary_ = nullptr;
    QLabel* detail_ = nullptr;
    QPushButton* button_ = nullptr;
    QFrame* streams_ = nullptr;
    QFrame* separator_ = nullptr;
    QWidget* slotBox_[2] = {};
    QToolButton* pauseBtn_[2] = {};
    QLabel* slotDot_[2] = {};
    QLabel* slotRate_[2] = {};
    SignalBars* slotBars_[2] = {};
    QLabel* slotIssue_[2] = {};
    bool paused_[2] = {};

    void SetPaused(int i, bool paused)
    {
        paused_[i] = paused;
        pauseBtn_[i]->setIcon(GlyphIcon(paused ? Glyph::Play : Glyph::Pause, IconColor(this), devicePixelRatioF()));
        pauseBtn_[i]->setToolTip(T(paused ? "Rust.Resume" : "Rust.Pause"));
    }

    // The streams (Live / Backup) of a target using the Rust engine, in an inset panel.
    void BuildStreams(QVBoxLayout* layout)
    {
        streams_ = new QFrame(card_);
        streams_->setObjectName("omrStreams");
        auto rows = new QVBoxLayout(streams_);
        rows->setContentsMargins(8, 2, 6, 2);
        rows->setSpacing(0);
        QLabel* names[2];
        for (int i = 0; i < 2; ++i) {
            const char* slot = i == 0 ? "primary" : "backup";
            auto box = slotBox_[i] = new QWidget(streams_);
            auto col = new QVBoxLayout(box);
            col->setContentsMargins(0, 2, 0, 2);
            col->setSpacing(0);
            auto row = new QHBoxLayout();
            row->setContentsMargins(0, 0, 0, 0);
            row->setSpacing(8);

            slotDot_[i] = new QLabel(box);
            slotDot_[i]->setFixedSize(8, 8);
            row->addWidget(slotDot_[i]);
            auto label = T(i == 0 ? "Rust.Primary" : "Rust.Backup");
            if (label.endsWith(':'))
                label.chop(1);
            names[i] = new QLabel(label, box);
            row->addWidget(names[i]);

            // Fixed width so both rows line up in columns.
            slotRate_[i] = new QLabel(box);
            slotRate_[i]->setMinimumWidth(slotRate_[i]->fontMetrics().horizontalAdvance("000.0 Mbps"));
            slotRate_[i]->setAlignment(Qt::AlignRight | Qt::AlignVCenter);
            row->addWidget(slotRate_[i]);
            row->addWidget(slotBars_[i] = new SignalBars(box));
            row->addStretch(1);

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
            col->addLayout(row);

            // What is wrong with this stream (or its history); wraps under the row.
            slotIssue_[i] = new QLabel(box);
            slotIssue_[i]->setFont(ScaledFont(font(), 0.88));
            slotIssue_[i]->setWordWrap(true);
            slotIssue_[i]->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Preferred);
            slotIssue_[i]->setContentsMargins(16, 0, 0, 2);
            slotIssue_[i]->hide();
            col->addWidget(slotIssue_[i]);
            rows->addWidget(box);

            if (i == 0) {
                separator_ = new QFrame(streams_);
                separator_->setFixedHeight(1);
                rows->addWidget(separator_);
            }
        }
        // Same width for "Live" and "Backup" (and their translations).
        int w = std::max(names[0]->sizeHint().width(), names[1]->sizeHint().width());
        names[0]->setMinimumWidth(w);
        names[1]->setMinimumWidth(w);
        layout->addWidget(streams_);
        streams_->hide();
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
        headerLayout->setContentsMargins(8, 8, 8, 4);
        headerLayout->setSpacing(2);
        auto bar = new QHBoxLayout();
        bar->setContentsMargins(0, 0, 0, 0);
        bar->setSpacing(6);
        // Start all (primary, blue) and Stop all with icon and text; Edit targets and
        // the advanced options as square icon buttons on the right.
        const QColor fg = palette().color(QPalette::WindowText);
        const qreal dpr = devicePixelRatioF();
        auto textButton = [header, dpr](Glyph g, const QColor& color, const char* key, const QString& style) {
            auto b = new QToolButton(header);
            b->setText(T(key));
            b->setToolTip(T(key));
            b->setIcon(GlyphIcon(g, color, dpr, 12));
            b->setIconSize(QSize(12, 12));
            b->setToolButtonStyle(Qt::ToolButtonTextBesideIcon);
            b->setSizePolicy(QSizePolicy::Fixed, QSizePolicy::Fixed);
            b->setCursor(Qt::PointingHandCursor);
            b->setStyleSheet(style);
            return b;
        };
        auto startAll = textButton(Glyph::Play, Qt::white, "Btn.StartAll",
                                   "QToolButton { background: #2f5bd3; color: #ffffff; border: none; border-radius: 6px;"
                                   " padding: 5px 10px; font-weight: bold; }"
                                   "QToolButton:hover { background: #3a68e0; }"
                                   "QToolButton:pressed { background: #2850bd; }");
        auto stopAll = textButton(Glyph::Stop, fg, "Btn.StopAll",
                                  "QToolButton { background: rgba(127, 127, 127, 0.18); border: 1px solid rgba(127, 127, 127, 0.35);"
                                  " border-radius: 6px; padding: 4px 10px; }"
                                  "QToolButton:hover { background: rgba(127, 127, 127, 0.30); }"
                                  "QToolButton:pressed { background: rgba(127, 127, 127, 0.42); }");
        auto edit = new QToolButton(header);
        edit->setIcon(GlyphIcon(Glyph::Edit, fg, dpr, 15));
        edit->setToolTip(T("Btn.EditTargets"));
        edit->setStyleSheet(SquareButtonStyle(QStringLiteral("rgba(127, 127, 127, 0.35)")));
        QObject::connect(startAll, &QToolButton::clicked, []() { Command({{"cmd", "start_all"}}); });
        QObject::connect(stopAll, &QToolButton::clicked, []() { Command({{"cmd", "stop_all"}}); });
        QObject::connect(edit, &QToolButton::clicked, [this]() { EditTargets(); });
        bar->addWidget(startAll);
        bar->addWidget(stopAll);
        bar->addStretch(1);
        bar->addWidget(edit);

        // Advanced options menu (next to Edit targets).
        advanced_ = new QToolButton(header);
        advanced_->setToolTip(T("Btn.Advanced"));
        advanced_->setPopupMode(QToolButton::InstantPopup);
        for (auto btn : {edit, advanced_}) {
            btn->setIconSize(QSize(15, 15));
            btn->setFixedSize(30, 30);
            btn->setCursor(Qt::PointingHandCursor);
        }
        SetAdvancedLook(false);
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

        list_ = new OutputsList(container_);
        list_->setDragDropMode(QAbstractItemView::InternalMove);
        list_->setSelectionMode(QAbstractItemView::SingleSelection);
        list_->setDropIndicatorShown(true);
        list_->setSizePolicy(QSizePolicy::Expanding, QSizePolicy::Maximum);
        list_->setSizeAdjustPolicy(QAbstractScrollArea::AdjustToContents);
        list_->setVerticalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
        list_->setHorizontalScrollBarPolicy(Qt::ScrollBarAsNeeded);
        list_->setStyleSheet("QListWidget { padding: 0px; margin: 0px; border: none; background: transparent; }"
                             "QListWidget::item { padding: 0px; margin: 0px; border: none; background: transparent; }"
                             "QListWidget::item:selected, QListWidget::item:active { border: none; background: transparent; }"
                             "QListWidget::item:hover, QListWidget::item:hover:selected, QListWidget::item:hover:active {"
                             " border: none; background: transparent; }");
        QObject::connect(list_->model(), &QAbstractItemModel::rowsMoved, [this]() { OnRowsMoved(); });
        layout->addWidget(list_);

        auto credit = new QLabel(
            QString::fromUtf8("This plugin is provided for free · Author: SoraYuki "
                              "(<a href=\"https://paypal.me/sorayuki0\">donate</a>)"),
            container_);
        credit->setTextFormat(Qt::RichText);
        credit->setTextInteractionFlags(Qt::TextBrowserInteraction);
        credit->setOpenExternalLinks(true);
        credit->setContentsMargins(10, 4, 10, 0); // aligned with the cards
        credit->setWordWrap(true);
        credit->setFont(ScaledFont(credit->font(), 0.88));
        credit->setStyleSheet(QString("QLabel { color: %1; }").arg(Rgba(palette().color(QPalette::WindowText), 0.6)));
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
        if (nullActive != advancedWarn_) {
            SetAdvancedLook(nullActive);
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
    bool advancedWarn_ = false;

    // Square icon button; amber while the null output is active (its option is in the menu).
    void SetAdvancedLook(bool warn)
    {
        advancedWarn_ = warn;
        const QColor color = warn ? QColor("#e6b800") : palette().color(QPalette::WindowText);
        advanced_->setIcon(GlyphIcon(Glyph::Options, color, devicePixelRatioF(), 15));
        advanced_->setStyleSheet(SquareButtonStyle(warn ? QStringLiteral("#e6b800") : QStringLiteral("rgba(127, 127, 127, 0.35)")));
    }

    static QString SquareButtonStyle(const QString& border)
    {
        return QString("QToolButton { background: transparent; border: 1px solid %1; border-radius: 6px; padding: 0px; }"
                       "QToolButton:hover { background: rgba(127, 127, 127, 0.22); }"
                       "QToolButton:pressed { background: rgba(127, 127, 127, 0.38); }"
                       "QToolButton::menu-indicator { image: none; width: 0px; }")
            .arg(border);
    }

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
