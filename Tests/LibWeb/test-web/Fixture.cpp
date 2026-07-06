/*
 * Copyright (c) 2024, Andrew Kaster <andrew@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include "Fixture.h"
#include "Application.h"

#include <AK/ByteBuffer.h>
#include <AK/LexicalPath.h>
#include <LibCore/Process.h>
#include <LibCore/StandardPaths.h>
#include <LibCore/System.h>

namespace TestWeb {

static ByteString s_fixtures_path;

// Key function for Fixture
Fixture::~Fixture() = default;

Optional<Fixture&> Fixture::lookup(StringView name)
{
    for (auto const& fixture : all()) {
        if (fixture->name() == name)
            return *fixture;
    }
    return {};
}

Vector<NonnullOwnPtr<Fixture>>& Fixture::all()
{
    static Vector<NonnullOwnPtr<Fixture>> fixtures;
    return fixtures;
}

class HttpEchoServerFixture final : public Fixture {
public:
    virtual ErrorOr<void> setup(WebView::WebContentOptions&) override;
    virtual void teardown_impl() override;
    virtual StringView name() const override { return "HttpEchoServer"sv; }
    virtual bool is_running() const override { return m_process.has_value(); }

private:
    ByteString m_script_path { "http-test-server.py" };
    Optional<Core::Process> m_process;
};

ErrorOr<void> HttpEchoServerFixture::setup(WebView::WebContentOptions& web_content_options)
{
    auto const script_path = LexicalPath::join(s_fixtures_path, m_script_path);
    auto const arguments = Vector { script_path.string(), "--directory", Application::the().test_root_path };

    // FIXME: Pick a more reasonable log path that is more observable
    auto const log_path = LexicalPath::join(Core::StandardPaths::tempfile_directory(), "http-test-server.log"sv).string();

    auto stdout_pipe = TRY(Core::System::create_pipe());

    auto const process_options = Core::ProcessSpawnOptions {
        .executable = Application::the().python_executable_path,
        .search_for_executable_in_path = true,
        .die_with_parent = true,
        .arguments = arguments,
        .file_actions = {
            Core::FileAction::OpenFile { ByteString::formatted("{}.stderr", log_path), Core::File::OpenMode::Write, Core::FileAction::StandardStream::Error },
            Core::FileAction::DupFd { stdout_pipe.write_end, Core::FileAction::StandardStream::Output } }
    };

    m_process = TRY(Core::Process::spawn(process_options));

    stdout_pipe.write_end.close();

    auto const stdout_file = MUST(Core::File::adopt_handle(move(stdout_pipe.read_end), Core::File::OpenMode::Read));

    auto buffer = MUST(ByteBuffer::create_uninitialized(5));
    TRY(stdout_file->read_some(buffer));

    auto const raw_output = ByteString { buffer, AK::ShouldChomp::NoChomp };

    if (auto const maybe_port = raw_output.to_number<u16>(); maybe_port.has_value())
        web_content_options.echo_server_port = maybe_port.value();
    else
        warnln("Failed to read echo server port from buffer: '{}'", raw_output);

    return {};
}

void HttpEchoServerFixture::teardown_impl()
{
    VERIFY(m_process.has_value());

    if (auto termination_or_error = m_process->terminate(); termination_or_error.is_error())
        warnln("Failed to terminate HTTP echo server, error: {}", termination_or_error.error());
    else if (auto wait_or_error = m_process->wait_for_termination(); wait_or_error.is_error())
        warnln("Failed to wait for HTTP echo server termination, error: {}", wait_or_error.error());

    m_process = {};
}

void Fixture::initialize_fixtures()
{
    s_fixtures_path = LexicalPath::join(Application::the().test_root_path, "Fixtures"sv).string();

    auto& registry = all();
    registry.append(make<HttpEchoServerFixture>());
}

}
