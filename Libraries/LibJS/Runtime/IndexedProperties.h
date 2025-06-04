/*
 * Copyright (c) 2020, Matthew Olsson <mattco@serenityos.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/NonnullOwnPtr.h>
#include <LibJS/Runtime/Shape.h>
#include <LibJS/Runtime/Value.h>

namespace JS {

constexpr size_t const SPARSE_ARRAY_HOLE_THRESHOLD = 200;
constexpr size_t const LENGTH_SETTER_GENERIC_STORAGE_THRESHOLD = 4 * MiB;

struct ValueAndAttributes {
    Value value;
    PropertyAttributes attributes { default_attributes };

    Optional<u32> property_offset {};
};

class IndexedProperties;
class IndexedPropertyIterator;

class IndexedPropertyStorage {
public:
    IndexedPropertyStorage() = default;
    explicit IndexedPropertyStorage(Vector<Value>&& initial_values);

    bool is_simple_storage() const { return m_is_simple_storage; }

    ALWAYS_INLINE bool has_index(u32 index) const
    {
        if (m_is_simple_storage) [[likely]] {
            return index < m_array_size && !m_packed_elements.data()[index].is_special_empty_value();
        }

        return m_sparse_elements.contains(index);
    }

    ALWAYS_INLINE Optional<ValueAndAttributes> get(u32 index) const
    {
        if (m_is_simple_storage) [[likely]] {
            if (has_index(index))
                return ValueAndAttributes { m_packed_elements.data()[index], default_attributes };
            return {};
        }

        if (index >= m_array_size)
            return {};
        return m_sparse_elements.get(index).copy();
    }

    ALWAYS_INLINE void put(u32 index, Value value, PropertyAttributes attributes = default_attributes)
    {
        if (m_is_simple_storage) [[likely]] {
            if (index >= m_array_size) {
                m_number_of_empty_elements += index - m_array_size;
                m_array_size = index + 1;
                grow_storage_if_needed();
            } else {
                if (m_packed_elements[index].is_special_empty_value()) {
                    --m_number_of_empty_elements;
                }
            }
            m_packed_elements[index] = value;
            if (value.is_special_empty_value()) {
                ++m_number_of_empty_elements;
            }
            return;
        }

        if (index >= m_array_size)
            m_array_size = index + 1;
        m_sparse_elements.set(index, { value, attributes });
    }
    void remove(u32 index);

    ValueAndAttributes take_first();
    ValueAndAttributes take_last();

    size_t size() const { return m_is_simple_storage ? m_packed_elements.size() : m_sparse_elements.size(); }

    size_t array_like_size() const { return m_array_size; }
    bool set_array_like_size(size_t new_size);

    auto const& sparse_elements() const { return m_sparse_elements; }
    Vector<Value> const& elements() const { return m_packed_elements; }

    bool has_empty_elements() const { return m_number_of_empty_elements.value() > 0; }

private:
    friend IndexedProperties;

    void grow_storage_if_needed();
    void switch_to_generic_storage();

    bool m_is_simple_storage { true };
    size_t m_array_size { 0 };
    Checked<size_t> m_number_of_empty_elements { 0 };
    Vector<Value> m_packed_elements;
    HashMap<u32, ValueAndAttributes> m_sparse_elements;
};

class IndexedPropertyIterator {
public:
    IndexedPropertyIterator(IndexedProperties const&, u32 starting_index, bool skip_empty);

    IndexedPropertyIterator& operator++();
    IndexedPropertyIterator& operator*();
    bool operator!=(IndexedPropertyIterator const&) const;

    u32 index() const { return m_index; }

private:
    void skip_empty_indices();

    IndexedProperties const& m_indexed_properties;
    Vector<u32> m_cached_indices;
    size_t m_next_cached_index { 0 };
    u32 m_index { 0 };
    bool m_skip_empty { false };
};

class IndexedProperties {
public:
    IndexedProperties() = default;

    explicit IndexedProperties(Vector<Value> values)
    {
        if (!values.is_empty())
            m_storage = make<IndexedPropertyStorage>(move(values));
    }

    bool has_index(u32 index) const { return m_storage ? m_storage->has_index(index) : false; }
    ALWAYS_INLINE Optional<ValueAndAttributes> get(u32 index) const
    {
        if (!m_storage)
            return {};
        return m_storage->get(index);
    }
    ALWAYS_INLINE void put(u32 index, Value value, PropertyAttributes attributes = default_attributes)
    {
        ensure_storage();
        if (m_storage->is_simple_storage() && (attributes != default_attributes || index > (array_like_size() + SPARSE_ARRAY_HOLE_THRESHOLD))) [[unlikely]] {
            m_storage->switch_to_generic_storage();
        }

        m_storage->put(index, value, attributes);
    }
    void remove(u32 index);

    void append(Value value, PropertyAttributes attributes = default_attributes) { put(array_like_size(), value, attributes); }

    IndexedPropertyIterator begin(bool skip_empty = true) const { return IndexedPropertyIterator(*this, 0, skip_empty); }
    IndexedPropertyIterator end() const { return IndexedPropertyIterator(*this, array_like_size(), false); }

    bool is_empty() const { return array_like_size() == 0; }
    size_t array_like_size() const { return m_storage ? m_storage->array_like_size() : 0; }
    bool set_array_like_size(size_t);

    IndexedPropertyStorage* storage() { return m_storage; }
    IndexedPropertyStorage const* storage() const { return m_storage; }

    size_t real_size() const;

    Vector<u32> indices() const;

    template<typename Callback>
    void for_each_value(Callback callback)
    {
        if (!m_storage)
            return;
        if (m_storage->is_simple_storage()) {
            for (auto& value : m_storage->elements())
                callback(value);
        } else {
            for (auto& element : m_storage->sparse_elements())
                callback(element.value.value);
        }
    }

private:
    void ensure_storage();

    OwnPtr<IndexedPropertyStorage> m_storage;
};

}
