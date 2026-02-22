/*
 * Copyright (c) 2023-2025, Tim Flynn <trflynn89@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibURL/Parser.h>
#include <LibWebView/Application.h>
#include <LibWebView/BookmarkStore.h>
#include <LibWebView/ViewImplementation.h>

#import <Application/ApplicationDelegate.h>
#import <Interface/InfoBar.h>
#import <Interface/LadybirdWebView.h>
#import <Interface/Menu.h>
#import <Interface/Tab.h>
#import <Interface/TabController.h>
#import <Utilities/Conversions.h>

#if !__has_feature(objc_arc)
#    error "This project requires ARC"
#endif

@interface ApplicationDelegate ()

@property (nonatomic, strong) NSMutableArray<TabController*>* managed_tabs;
@property (nonatomic, weak) Tab* active_tab;

@property (nonatomic, strong) InfoBar* info_bar;
@property (nonatomic, strong) NSMenu* bookmarks_submenu;

- (NSMenuItem*)createApplicationMenu;
- (NSMenuItem*)createFileMenu;
- (NSMenuItem*)createEditMenu;
- (NSMenuItem*)createViewMenu;
- (NSMenuItem*)createBookmarksMenu;
- (NSMenuItem*)createHistoryMenu;
- (NSMenuItem*)createInspectMenu;
- (NSMenuItem*)createDebugMenu;
- (NSMenuItem*)createWindowMenu;
- (NSMenuItem*)createHelpMenu;

- (void)rebuildBookmarksMenu;

@end

@implementation ApplicationDelegate

- (instancetype)init
{
    if (self = [super init]) {
        [NSApp setMainMenu:[[NSMenu alloc] init]];

        [[NSApp mainMenu] addItem:[self createApplicationMenu]];
        [[NSApp mainMenu] addItem:[self createFileMenu]];
        [[NSApp mainMenu] addItem:[self createEditMenu]];
        [[NSApp mainMenu] addItem:[self createViewMenu]];
        [[NSApp mainMenu] addItem:[self createBookmarksMenu]];
        [[NSApp mainMenu] addItem:[self createHistoryMenu]];
        [[NSApp mainMenu] addItem:[self createInspectMenu]];
        [[NSApp mainMenu] addItem:[self createDebugMenu]];
        [[NSApp mainMenu] addItem:[self createWindowMenu]];
        [[NSApp mainMenu] addItem:[self createHelpMenu]];

        self.managed_tabs = [[NSMutableArray alloc] init];

        // Reduce the tooltip delay, as the default delay feels quite long.
        [[NSUserDefaults standardUserDefaults] setObject:@100 forKey:@"NSInitialToolTipDelay"];
    }

    return self;
}

#pragma mark - Public methods

- (nonnull TabController*)createNewTab:(Web::HTML::ActivateTab)activate_tab
                               fromTab:(nullable Tab*)tab
{
    auto* controller = [[TabController alloc] init];
    [self initializeTabController:controller
                      activateTab:activate_tab
                          fromTab:tab];

    return controller;
}

- (TabController*)createNewTab:(Optional<URL::URL> const&)url
                       fromTab:(Tab*)tab
                   activateTab:(Web::HTML::ActivateTab)activate_tab
{
    auto* controller = [self createNewTab:activate_tab fromTab:tab];

    if (url.has_value()) {
        [controller loadURL:*url];
    }

    return controller;
}

- (nonnull TabController*)createChildTab:(Optional<URL::URL> const&)url
                                 fromTab:(nonnull Tab*)tab
                             activateTab:(Web::HTML::ActivateTab)activate_tab
                               pageIndex:(u64)page_index
{
    auto* controller = [self createChildTab:activate_tab fromTab:tab pageIndex:page_index];

    if (url.has_value()) {
        [controller loadURL:*url];
    }

    return controller;
}

- (void)setActiveTab:(Tab*)tab
{
    self.active_tab = tab;

    if (self.info_bar) {
        [self.info_bar tabBecameActive:self.active_tab];
    }
}

- (Tab*)activeTab
{
    return self.active_tab;
}

- (void)removeTab:(TabController*)controller
{
    [self.managed_tabs removeObject:controller];
}

- (void)onDevtoolsEnabled
{
    if (!self.info_bar) {
        self.info_bar = [[InfoBar alloc] init];
    }

    auto message = MUST(String::formatted("DevTools is enabled on port {}", WebView::Application::browser_options().devtools_port));

    [self.info_bar showWithMessage:Ladybird::string_to_ns_string(message)
                dismissButtonTitle:@"Disable"
              dismissButtonClicked:^{
                  MUST(WebView::Application::the().toggle_devtools_enabled());
              }
                         activeTab:self.active_tab];
}

- (void)onDevtoolsDisabled
{
    if (self.info_bar) {
        [self.info_bar hide];
        self.info_bar = nil;
    }
}

#pragma mark - Private methods

- (void)openLocation:(id)sender
{
    auto* current_tab = [NSApp keyWindow];

    if (![current_tab isKindOfClass:[Tab class]]) {
        return;
    }

    auto* controller = (TabController*)[current_tab windowController];
    [controller focusLocationToolbarItem];
}

- (nonnull TabController*)createChildTab:(Web::HTML::ActivateTab)activate_tab
                                 fromTab:(nonnull Tab*)tab
                               pageIndex:(u64)page_index
{
    auto* controller = [[TabController alloc] initAsChild:tab pageIndex:page_index];
    [self initializeTabController:controller
                      activateTab:activate_tab
                          fromTab:tab];

    return controller;
}

- (void)initializeTabController:(TabController*)controller
                    activateTab:(Web::HTML::ActivateTab)activate_tab
                        fromTab:(nullable Tab*)tab
{
    [controller showWindow:nil];

    if (tab) {
        [[tab tabGroup] addWindow:controller.window];

        // FIXME: Can we create the tabbed window above without it becoming active in the first place?
        if (activate_tab == Web::HTML::ActivateTab::No) {
            [tab orderFront:nil];
        }
    }

    if (activate_tab == Web::HTML::ActivateTab::Yes) {
        [[controller window] orderFrontRegardless];
        [controller focusLocationToolbarItem];
    }

    [self.managed_tabs addObject:controller];
}

- (void)closeCurrentTab:(id)sender
{
    auto* current_window = [NSApp keyWindow];
    [current_window performClose:self];
}

- (void)clearHistory:(id)sender
{
    for (TabController* controller in self.managed_tabs) {
        [controller clearHistory];
    }
}

- (NSMenuItem*)createApplicationMenu
{
    auto* menu = [[NSMenuItem alloc] init];

    auto* process_name = [[NSProcessInfo processInfo] processName];
    auto* submenu = [[NSMenu alloc] initWithTitle:process_name];

    [submenu addItem:Ladybird::create_application_menu_item(WebView::Application::the().open_about_page_action())];
    [submenu addItem:[NSMenuItem separatorItem]];

    [submenu addItem:Ladybird::create_application_menu_item(WebView::Application::the().open_settings_page_action())];
    [submenu addItem:[NSMenuItem separatorItem]];

    [submenu addItem:[[NSMenuItem alloc] initWithTitle:[NSString stringWithFormat:@"Hide %@", process_name]
                                                action:@selector(hide:)
                                         keyEquivalent:@"h"]];
    [submenu addItem:[NSMenuItem separatorItem]];

    [submenu addItem:[[NSMenuItem alloc] initWithTitle:[NSString stringWithFormat:@"Quit %@", process_name]
                                                action:@selector(terminate:)
                                         keyEquivalent:@"q"]];

    [menu setSubmenu:submenu];
    return menu;
}

- (NSMenuItem*)createFileMenu
{
    auto* menu = [[NSMenuItem alloc] init];
    auto* submenu = [[NSMenu alloc] initWithTitle:@"File"];

    [submenu addItem:[[NSMenuItem alloc] initWithTitle:@"New Tab"
                                                action:@selector(createNewTab:)
                                         keyEquivalent:@"t"]];
    [submenu addItem:[[NSMenuItem alloc] initWithTitle:@"Close Tab"
                                                action:@selector(closeCurrentTab:)
                                         keyEquivalent:@"w"]];
    [submenu addItem:[NSMenuItem separatorItem]];

    [submenu addItem:[[NSMenuItem alloc] initWithTitle:@"Open Location"
                                                action:@selector(openLocation:)
                                         keyEquivalent:@"l"]];

    [menu setSubmenu:submenu];
    return menu;
}

- (NSMenuItem*)createEditMenu
{
    auto* menu = [[NSMenuItem alloc] init];
    auto* submenu = [[NSMenu alloc] initWithTitle:@"Edit"];

    [submenu addItem:[[NSMenuItem alloc] initWithTitle:@"Undo"
                                                action:@selector(undo:)
                                         keyEquivalent:@"z"]];
    [submenu addItem:[[NSMenuItem alloc] initWithTitle:@"Redo"
                                                action:@selector(redo:)
                                         keyEquivalent:@"y"]];
    [submenu addItem:[NSMenuItem separatorItem]];

    [submenu addItem:[[NSMenuItem alloc] initWithTitle:@"Cut"
                                                action:@selector(cut:)
                                         keyEquivalent:@"x"]];

    [submenu addItem:Ladybird::create_application_menu_item(WebView::Application::the().copy_selection_action())];
    [submenu addItem:Ladybird::create_application_menu_item(WebView::Application::the().paste_action())];
    [submenu addItem:[NSMenuItem separatorItem]];

    [submenu addItem:Ladybird::create_application_menu_item(WebView::Application::the().select_all_action())];
    [submenu addItem:[NSMenuItem separatorItem]];

    [submenu addItem:[[NSMenuItem alloc] initWithTitle:@"Find..."
                                                action:@selector(find:)
                                         keyEquivalent:@"f"]];
    [submenu addItem:[[NSMenuItem alloc] initWithTitle:@"Find Next"
                                                action:@selector(findNextMatch:)
                                         keyEquivalent:@"g"]];
    [submenu addItem:[[NSMenuItem alloc] initWithTitle:@"Find Previous"
                                                action:@selector(findPreviousMatch:)
                                         keyEquivalent:@"G"]];
    [submenu addItem:[[NSMenuItem alloc] initWithTitle:@"Use Selection for Find"
                                                action:@selector(useSelectionForFind:)
                                         keyEquivalent:@"e"]];

    [menu setSubmenu:submenu];
    return menu;
}

- (NSMenuItem*)createViewMenu
{
    auto* menu = [[NSMenuItem alloc] init];
    auto* submenu = [[NSMenu alloc] initWithTitle:@"View"];

    auto* zoom_menu = Ladybird::create_application_menu(WebView::Application::the().zoom_menu());
    auto* zoom_menu_item = [[NSMenuItem alloc] initWithTitle:[zoom_menu title]
                                                      action:nil
                                               keyEquivalent:@""];
    [zoom_menu_item setSubmenu:zoom_menu];

    auto* color_scheme_menu = Ladybird::create_application_menu(WebView::Application::the().color_scheme_menu());
    auto* color_scheme_menu_item = [[NSMenuItem alloc] initWithTitle:[color_scheme_menu title]
                                                              action:nil
                                                       keyEquivalent:@""];
    [color_scheme_menu_item setSubmenu:color_scheme_menu];

    auto* contrast_menu = Ladybird::create_application_menu(WebView::Application::the().contrast_menu());
    auto* contrast_menu_item = [[NSMenuItem alloc] initWithTitle:[contrast_menu title]
                                                          action:nil
                                                   keyEquivalent:@""];
    [contrast_menu_item setSubmenu:contrast_menu];

    auto* motion_menu = Ladybird::create_application_menu(WebView::Application::the().motion_menu());
    auto* motion_menu_item = [[NSMenuItem alloc] initWithTitle:[motion_menu title]
                                                        action:nil
                                                 keyEquivalent:@""];
    [motion_menu_item setSubmenu:motion_menu];

    [submenu addItem:zoom_menu_item];
    [submenu addItem:[NSMenuItem separatorItem]];
    [submenu addItem:color_scheme_menu_item];
    [submenu addItem:contrast_menu_item];
    [submenu addItem:motion_menu_item];
    [submenu addItem:[NSMenuItem separatorItem]];

    [menu setSubmenu:submenu];
    return menu;
}

- (NSMenuItem*)createBookmarksMenu
{
    auto* menu = [[NSMenuItem alloc] init];
    auto* submenu = [[NSMenu alloc] initWithTitle:@"Bookmarks"];

    [submenu addItem:[[NSMenuItem alloc] initWithTitle:@"Add Bookmark"
                                                action:@selector(toggleBookmark:)
                                         keyEquivalent:@"d"]];
    [submenu addItem:[NSMenuItem separatorItem]];

    self.bookmarks_submenu = submenu;
    [self rebuildBookmarksMenu];

    WebView::Application::bookmark_store().on_bookmarks_changed = [self] {
        [self rebuildBookmarksMenu];
    };

    [menu setSubmenu:submenu];
    return menu;
}

- (void)rebuildBookmarksMenu
{
    // Remove all items after the separator (index 2+).
    while ([self.bookmarks_submenu numberOfItems] > 2)
        [self.bookmarks_submenu removeItemAtIndex:2];

    auto const& bookmarks = WebView::Application::bookmark_store().bookmarks();
    for (auto const& bookmark : bookmarks) {
        auto title = bookmark.title.is_empty()
            ? bookmark.url.serialize()
            : bookmark.title;

        auto* item = [[NSMenuItem alloc] initWithTitle:Ladybird::string_to_ns_string(title)
                                                action:@selector(openBookmark:)
                                         keyEquivalent:@""];
        [item setRepresentedObject:Ladybird::string_to_ns_string(bookmark.url.serialize())];
        [self.bookmarks_submenu addItem:item];
    }
}

- (void)toggleBookmark:(id)sender
{
    auto* current_window = [NSApp keyWindow];
    if (![current_window isKindOfClass:[Tab class]])
        return;

    auto* tab = (Tab*)current_window;
    auto& view = [[tab web_view] view];

    auto const& url = view.url();
    auto& store = WebView::Application::bookmark_store();

    if (store.is_bookmarked(url)) {
        store.remove_bookmark(url);
    } else {
        auto const& title = view.title();
        auto title_string = title.to_well_formed_utf8();

        if (title_string.is_empty())
            store.add_bookmark(url, url.serialize());
        else
            store.add_bookmark(url, title_string);
    }
}

- (void)openBookmark:(id)sender
{
    auto* url_string = (NSString*)[sender representedObject];
    auto url = URL::Parser::basic_parse(Ladybird::ns_string_to_string(url_string));
    if (!url.has_value())
        return;

    auto* current_window = [NSApp keyWindow];
    if (![current_window isKindOfClass:[Tab class]])
        return;

    auto* controller = (TabController*)[current_window windowController];
    [controller loadURL:url.release_value()];
}

- (NSMenuItem*)createHistoryMenu
{
    auto* menu = [[NSMenuItem alloc] init];

    auto* submenu = [[NSMenu alloc] initWithTitle:@"History"];
    [submenu setAutoenablesItems:NO];

    [submenu addItem:Ladybird::create_application_menu_item(WebView::Application::the().reload_action())];
    [submenu addItem:[NSMenuItem separatorItem]];

    [submenu addItem:[[NSMenuItem alloc] initWithTitle:@"Clear History"
                                                action:@selector(clearHistory:)
                                         keyEquivalent:@""]];

    [menu setSubmenu:submenu];
    return menu;
}

- (NSMenuItem*)createInspectMenu
{
    auto* menu = [[NSMenuItem alloc] init];

    auto* submenu = Ladybird::create_application_menu(WebView::Application::the().inspect_menu());
    [menu setSubmenu:submenu];

    return menu;
}

- (NSMenuItem*)createDebugMenu
{
    auto* menu = [[NSMenuItem alloc] init];

    auto* submenu = Ladybird::create_application_menu(WebView::Application::the().debug_menu());
    [menu setSubmenu:submenu];

    return menu;
}

- (NSMenuItem*)createWindowMenu
{
    auto* menu = [[NSMenuItem alloc] init];
    auto* submenu = [[NSMenu alloc] initWithTitle:@"Window"];

    [NSApp setWindowsMenu:submenu];

    [menu setSubmenu:submenu];
    return menu;
}

- (NSMenuItem*)createHelpMenu
{
    auto* menu = [[NSMenuItem alloc] init];
    auto* submenu = [[NSMenu alloc] initWithTitle:@"Help"];

    [NSApp setHelpMenu:submenu];

    [menu setSubmenu:submenu];
    return menu;
}

#pragma mark - NSApplicationDelegate

- (void)applicationDidFinishLaunching:(NSNotification*)notification
{
    auto const& browser_options = WebView::Application::browser_options();

    if (browser_options.devtools_port.has_value())
        [self onDevtoolsEnabled];

    Tab* tab = nil;

    for (auto const& url : browser_options.urls) {
        auto activate_tab = tab == nil ? Web::HTML::ActivateTab::Yes : Web::HTML::ActivateTab::No;

        auto* controller = [self createNewTab:url
                                      fromTab:tab
                                  activateTab:activate_tab];

        tab = (Tab*)[controller window];
    }
}

- (void)applicationWillTerminate:(NSNotification*)notification
{
}

- (BOOL)applicationShouldTerminateAfterLastWindowClosed:(NSApplication*)sender
{
    return YES;
}

- (void)applicationDidChangeScreenParameters:(NSNotification*)notification
{
    for (TabController* controller in self.managed_tabs) {
        auto* tab = (Tab*)[controller window];
        [[tab web_view] handleDisplayRefreshRateChange];
    }
}

- (BOOL)validateMenuItem:(NSMenuItem*)menu
{
    SEL action = [menu action];

    if (action == @selector(closeCurrentTab:)) {
        return [[NSApp keyWindow] isKindOfClass:[Tab class]];
    }

    if (action == @selector(toggleBookmark:)) {
        auto* current_window = [NSApp keyWindow];
        if (![current_window isKindOfClass:[Tab class]])
            return NO;

        auto* tab = (Tab*)current_window;
        auto const& url = [[tab web_view] view].url();

        if (WebView::Application::bookmark_store().is_bookmarked(url))
            [menu setTitle:@"Remove Bookmark"];
        else
            [menu setTitle:@"Add Bookmark"];

        return YES;
    }

    return YES;
}

@end
