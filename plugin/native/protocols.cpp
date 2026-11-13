// Protocol table for the editor: the source of truth is Rust (src/protocols.rs).
#include "protocols.h"
#include "json.hpp"

#include <deque>
#include <string>
#include <vector>

extern "C" char* omr_protocols_json();
extern "C" void omr_free(char*);

namespace {

class RustProtocolInfos : public ProtocolInfos {
    std::deque<std::string> strings_; // stable addresses for the const char* pointers
    std::vector<ProtocolInfo> list_;

    const char* keep(const nlohmann::json& v)
    {
        if (!v.is_string())
            return nullptr;
        strings_.push_back(v.get<std::string>());
        return strings_.back().c_str();
    }

public:
    RustProtocolInfos()
    {
        char* raw = omr_protocols_json();
        auto parsed = nlohmann::json::parse(raw ? raw : "[]", nullptr, false);
        omr_free(raw);
        if (parsed.is_array()) {
            for (auto& p : parsed)
                list_.push_back({keep(p["protocol"]), keep(p["label"]), keep(p["output_id"]), keep(p["service_id"])});
        }
        list_.push_back({nullptr, nullptr, nullptr, nullptr}); // terminator, like the original table
    }

    const ProtocolInfo* GetInfo(const char* protocol) override
    {
        std::string_view wanted{protocol ? protocol : ""};
        for (auto p = list_.data(); p->protocol; ++p) {
            if (wanted == p->protocol)
                return p;
        }
        return nullptr;
    }

    const ProtocolInfo* GetList() override { return list_.data(); }
};

} // namespace

ProtocolInfos* GetProtocolInfos()
{
    static RustProtocolInfos impl;
    return &impl;
}
