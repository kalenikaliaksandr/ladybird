/*
 * Copyright (c) 2020, Matthew Olsson <mattco@serenityos.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/QuickSort.h>
#include <LibJS/Runtime/Accessor.h>
#include <LibJS/Runtime/IndexedProperties.h>

namespace JS {

void IndexedPropertyStorage::grow_storage_if_needed()
{
    if (m_array_size <= m_packed_elements.size())
        return;

    if (m_array_size <= m_packed_elements.capacity()) {
        m_packed_elements.resize_with_default_value_and_keep_capacity(m_array_size, js_special_empty_value());
    } else {
        // When the array is actually full grow storage by 25% at a time.
        m_packed_elements.resize_with_default_value_and_keep_capacity(m_array_size + (m_array_size / 4), js_special_empty_value());
    }
}

void IndexedPropertyStorage::remove(u32 index)
{
    if (m_is_simple_storage) {
        VERIFY(index < m_array_size);
        ++m_number_of_empty_elements;
        m_packed_elements[index] = js_special_empty_value();
        return;
    }

    VERIFY(index < m_array_size);
    m_sparse_elements.remove(index);
}

ValueAndAttributes IndexedPropertyStorage::take_first()
{
    if (m_is_simple_storage) {
        if (m_packed_elements.first().is_special_empty_value()) {
            --m_number_of_empty_elements;
        }
        m_array_size--;
        return { m_packed_elements.take_first(), default_attributes };
    }

    VERIFY(m_array_size > 0);
    m_array_size--;

    auto indices = m_sparse_elements.keys();
    quick_sort(indices);

    auto it = m_sparse_elements.find(indices.first());
    auto first_element = it->value;
    m_sparse_elements.remove(it);
    return first_element;
}

ValueAndAttributes IndexedPropertyStorage::take_last()
{
    if (m_is_simple_storage) {
        m_array_size--;
        auto last_element = m_packed_elements[m_array_size];
        if (last_element.is_special_empty_value()) {
            --m_number_of_empty_elements;
        }
        m_packed_elements[m_array_size] = js_special_empty_value();
        return { last_element, default_attributes };
    }

    VERIFY(m_array_size > 0);
    m_array_size--;

    auto result = m_sparse_elements.get(m_array_size);
    if (!result.has_value())
        return {};
    m_sparse_elements.remove(m_array_size);
    return result.value();
}

bool IndexedPropertyStorage::set_array_like_size(size_t new_size)
{
    if (new_size == m_array_size)
        return true;

    if (m_is_simple_storage) {
        auto old_size = m_array_size;
        m_array_size = new_size;
        m_packed_elements.resize_with_default_value_and_keep_capacity(new_size, js_special_empty_value());

        if (old_size <= m_array_size) {
            m_number_of_empty_elements += m_array_size - old_size;
        } else {
            m_number_of_empty_elements = 0;
            for (auto& value : m_packed_elements) {
                if (value.is_special_empty_value())
                    ++m_number_of_empty_elements;
            }
        }

        return true;
    }

    if (new_size >= m_array_size) {
        m_array_size = new_size;
        return true;
    }

    bool any_failed = false;
    size_t highest_index = 0;

    HashMap<u32, ValueAndAttributes> new_sparse_elements;
    for (auto& entry : m_sparse_elements) {
        if (entry.key >= new_size) {
            if (entry.value.attributes.is_configurable())
                continue;
            any_failed = true;
        }
        new_sparse_elements.set(entry.key, entry.value);
        highest_index = max(highest_index, entry.key);
    }

    if (any_failed)
        m_array_size = highest_index + 1;
    else
        m_array_size = new_size;

    m_sparse_elements = move(new_sparse_elements);
    return !any_failed;
}

void IndexedPropertyStorage::switch_to_generic_storage()
{
    VERIFY(m_is_simple_storage);
    for (size_t i = 0; i < m_packed_elements.size(); ++i) {
        auto value = m_packed_elements[i];
        if (!value.is_special_empty_value())
            m_sparse_elements.set(i, { value, default_attributes });
    }
    m_is_simple_storage = false;
}

IndexedPropertyIterator::IndexedPropertyIterator(IndexedProperties const& indexed_properties, u32 staring_index, bool skip_empty)
    : m_indexed_properties(indexed_properties)
    , m_index(staring_index)
    , m_skip_empty(skip_empty)
{
    if (m_skip_empty) {
        m_cached_indices = m_indexed_properties.indices();
        skip_empty_indices();
    }
}

IndexedPropertyIterator& IndexedPropertyIterator::operator++()
{
    m_index++;

    if (m_skip_empty)
        skip_empty_indices();

    return *this;
}

IndexedPropertyIterator& IndexedPropertyIterator::operator*()
{
    return *this;
}

bool IndexedPropertyIterator::operator!=(IndexedPropertyIterator const& other) const
{
    return m_index != other.m_index;
}

void IndexedPropertyIterator::skip_empty_indices()
{
    for (size_t i = m_next_cached_index; i < m_cached_indices.size(); i++) {
        auto index = m_cached_indices[i];
        if (index < m_index)
            continue;
        m_index = index;
        m_next_cached_index = i + 1;
        return;
    }
    m_index = m_indexed_properties.array_like_size();
}

void IndexedProperties::remove(u32 index)
{
    VERIFY(m_storage);
    VERIFY(m_storage->has_index(index));
    m_storage->remove(index);
}

bool IndexedProperties::set_array_like_size(size_t new_size)
{
    ensure_storage();
    auto current_array_like_size = array_like_size();

    // We can't use simple storage for lengths that don't fit in an i32.
    // Also, to avoid gigantic unused storage allocations, let's put an (arbitrary) 4M cap on simple storage here.
    // This prevents something like "a = []; a.length = 0x80000000;" from allocating 2G entries.
    if (m_storage->is_simple_storage()
        && (new_size > NumericLimits<i32>::max()
            || (current_array_like_size < LENGTH_SETTER_GENERIC_STORAGE_THRESHOLD && new_size > LENGTH_SETTER_GENERIC_STORAGE_THRESHOLD))) {
        m_storage->switch_to_generic_storage();
    }

    return m_storage->set_array_like_size(new_size);
}

size_t IndexedProperties::real_size() const
{
    if (!m_storage)
        return 0;
    if (m_storage->is_simple_storage()) {
        auto const& packed_elements = m_storage->elements();
        size_t size = 0;
        for (auto const& element : packed_elements) {
            if (!element.is_special_empty_value())
                ++size;
        }
        return size;
    }

    return m_storage->size();
}

Vector<u32> IndexedProperties::indices() const
{
    if (!m_storage)
        return {};
    if (m_storage->is_simple_storage()) {
        auto const& storage = *m_storage;
        auto const& elements = storage.elements();
        Vector<u32> indices;
        indices.ensure_capacity(storage.array_like_size());
        for (size_t i = 0; i < elements.size(); ++i) {
            if (!elements.at(i).is_special_empty_value())
                indices.unchecked_append(i);
        }
        return indices;
    }

    auto indices = m_storage->sparse_elements().keys();
    quick_sort(indices);
    return indices;
}

void IndexedProperties::ensure_storage()
{
    if (!m_storage)
        m_storage = make<IndexedPropertyStorage>();
}

}
