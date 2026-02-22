set(TEST_SOURCES
    TestBookmarkStore.cpp
    TestWebViewURL.cpp
)

foreach(source IN LISTS TEST_SOURCES)
    ladybird_test("${source}" LibWebView LIBS LibWebView LibURL)
endforeach()
