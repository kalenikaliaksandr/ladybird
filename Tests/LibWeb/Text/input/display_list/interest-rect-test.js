// Testing recordings around the scroll positions, and what they did with given producers. Output is buffered:
// println() changes the document, which would make the next recording record again what a test checks.

function recordAroundScrollPositions() {
    internals.beginDisplayListTrace();
    internals.recordDisplayListForTesting(false, false, "around-scroll-positions");
    return internals.takeDisplayListTrace();
}

function recordWholeDocument() {
    internals.beginDisplayListTrace();
    internals.recordDisplayListForTesting(false, false, "whole-document");
    return internals.takeDisplayListTrace();
}

// What a traced recording did with a producer of the element with the given id, such as its "background": RECORD,
// COPY, CULL or SKIP, or "untouched" where the recording copied a whole scope around it.
function producerAction(trace, id, producer) {
    const label = new RegExp(`#${id}(\\.[^/]*)?/${producer}$`);
    for (const line of trace.split("\n")) {
        const [name, action] = line.trim().split(" ");
        if (label.test(name)) return action;
    }
    return "untouched";
}

function interestRectTest(body) {
    promiseTest(async () => {
        const output = [];
        try {
            if (document.readyState !== "complete") {
                await new Promise(resolve => window.addEventListener("load", resolve, { once: true }));
            }
            await document.fonts.ready;
            __outputElement.style.display = "none";
            // Whatever rendered before, the recordings a test checks copy from this one.
            internals.recordDisplayListForTesting(false, false, "whole-document");
            body(line => output.push(line));
        } catch (error) {
            output.push(`FAIL: ${error}`);
        } finally {
            __outputElement.style.display = "";
            for (const line of output) println(line);
        }
    });
}
