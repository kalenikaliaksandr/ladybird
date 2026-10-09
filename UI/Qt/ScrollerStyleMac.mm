/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWebView/Application.h>
#include <UI/Qt/ScrollerStyleMac.h>

#import <Cocoa/Cocoa.h>

namespace Ladybird {

static Web::ScrollbarStyle preferred_scrollbar_style()
{
    if (NSScroller.preferredScrollerStyle == NSScrollerStyleOverlay)
        return Web::ScrollbarStyle::Overlay;
    return Web::ScrollbarStyle::Classic;
}

void install_preferred_scroller_style_observer()
{
    WebView::Application::the().set_system_scrollbar_style(preferred_scrollbar_style());

    [NSNotificationCenter.defaultCenter addObserverForName:NSPreferredScrollerStyleDidChangeNotification
                                                    object:nil
                                                     queue:NSOperationQueue.mainQueue
                                                usingBlock:^(NSNotification*) {
                                                    WebView::Application::the().set_system_scrollbar_style(preferred_scrollbar_style());
                                                }];
}

}
