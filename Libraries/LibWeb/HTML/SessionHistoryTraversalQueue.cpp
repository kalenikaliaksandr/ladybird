/*
 * Copyright (c) 2024, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/HTML/Navigable.h>
#include <LibWeb/HTML/SessionHistoryTraversalQueue.h>

namespace Web::HTML {

GC_DEFINE_ALLOCATOR(SessionHistoryTraversalQueue);
GC_DEFINE_ALLOCATOR(SessionHistoryTraversalQueueEntry);

GC::Ref<SessionHistoryTraversalQueueEntry> SessionHistoryTraversalQueueEntry::create(JS::VM& vm, GC::Ref<SessionHistoryTraversalSteps> steps, GC::Ptr<HTML::Navigable> target_navigable)
{
    return vm.heap().allocate<SessionHistoryTraversalQueueEntry>(steps, target_navigable);
}

void SessionHistoryTraversalQueueEntry::visit_edges(JS::Cell::Visitor& visitor)
{
    Base::visit_edges(visitor);
    visitor.visit(m_steps);
    visitor.visit(m_target_navigable);
}

// void SessionHistoryTraversalQueueEntry::execute_steps_with_completion(GC::Ref<GC::Function<void()>> callback) const
// {
//     auto promise = Core::Promise<Empty>::construct();
//     m_steps->function()(promise);
//     promise->when_resolved([callback = GC::Root { callback }](auto&) {
//         callback->function()();
//     });
//     promise->when_rejected([callback = GC::Root { callback }](auto&) {
//         callback->function()();
//     });
// }

SessionHistoryTraversalQueue::SessionHistoryTraversalQueue()
{
    // m_timer = Core::Timer::create_single_shot(0, [this] {
    //     if (m_is_task_running && m_queue.size() > 0) {
    //         m_timer->start();
    //         return;
    //     }
    //     while (m_queue.size() > 0) {
    //         m_is_task_running = true;
    //         auto entry = m_queue.take_first();
    //         auto promise = Core::Promise<Empty>::construct();
    //         entry->execute_steps(promise);
    //         m_is_task_running = false;
    //     }
    // });
}

void SessionHistoryTraversalQueue::visit_edges(JS::Cell::Visitor& visitor)
{
    Base::visit_edges(visitor);
    visitor.visit(m_queue);
}

void SessionHistoryTraversalQueue::process()
{
    // m_current_promise.clear();
    VERIFY(!m_is_task_running);
    if (m_queue.is_empty())
        return;
    auto task = m_queue.take_first();

    auto promise = Core::Promise<Empty>::construct();

    auto timeout = Core::Timer::create_single_shot(10 * 1000, [promise] {
        dbgln(">SHTQ JOB TIMEOUT");
        promise->resolve({});
        VERIFY_NOT_REACHED();
    });

    promise->when_resolved([weak_this = GC::Weak { *this }, timeout](auto&) {
        timeout->stop();
        // dbgln(">when resolved");
        if (!weak_this)
            return;
        weak_this->m_is_task_running = false;
        // dbgln(">queue size after resolve: {}", weak_this->m_queue.size());
        weak_this->process();
    });
    promise->when_rejected([weak_this = GC::Weak { *this }, timeout](auto&) {
        timeout->stop();
        // dbgln(">when rejected");
        if (!weak_this)
            return;
        weak_this->m_is_task_running = false;
        weak_this->process();
    });

    timeout->start();

    m_is_task_running = true;
    task->execute_steps(promise);
}

void SessionHistoryTraversalQueue::append(GC::Ref<SessionHistoryTraversalSteps> steps)
{
    m_queue.append(SessionHistoryTraversalQueueEntry::create(vm(), steps, nullptr));
    // if (!m_timer->is_active()) {
    //     m_timer->start();
    // }
    if (!m_is_task_running)
        process();
}

void SessionHistoryTraversalQueue::append_sync(GC::Ref<SessionHistoryTraversalSteps> steps, GC::Ptr<Navigable> target_navigable)
{
    m_queue.append(SessionHistoryTraversalQueueEntry::create(vm(), steps, target_navigable));
    // if (!m_timer->is_active()) {
    //     m_timer->start();
    // }
    if (!m_is_task_running)
        process();
}

void SessionHistoryTraversalQueue::execute_all_sync_steps(HashTable<GC::Ref<Navigable>> const& set, GC::Root<GC::Function<void()>> callback)
{
    auto task = first_synchronous_navigation_steps_with_target_navigable_not_contained_in(set);
    if (!task) {
        callback->function()();
        return;
    }

    auto promise = Core::Promise<Empty>::construct();

    auto timeout = Core::Timer::create_single_shot(10 * 1000, [promise] {
        dbgln(">SHTQ JOB TIMEOUT");
        promise->resolve({});
        VERIFY_NOT_REACHED();
    });

    promise->when_resolved([weak_this = GC::Weak { *this }, timeout, set, callback](auto&) {
        timeout->stop();
        if (!weak_this)
            return;
        weak_this->m_is_task_running = false;
        weak_this->execute_all_sync_steps(set, callback);
    });
    promise->when_rejected([weak_this = GC::Weak { *this }, timeout, set, callback](auto&) {
        timeout->stop();
        if (!weak_this)
            return;
        weak_this->m_is_task_running = false;
        weak_this->execute_all_sync_steps(set, callback);
    });

    timeout->start();

    m_is_task_running = true;
    task->execute_steps(promise);
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#sync-navigations-jump-queue
GC::Ptr<SessionHistoryTraversalQueueEntry> SessionHistoryTraversalQueue::first_synchronous_navigation_steps_with_target_navigable_not_contained_in(HashTable<GC::Ref<Navigable>> const& set)
{
    auto index = m_queue.find_first_index_if([&set](auto const& entry) -> bool {
        return (entry->target_navigable() != nullptr) && !set.contains(*entry->target_navigable());
    });
    if (index.has_value())
        return m_queue.take(*index);
    return {};
}

}
