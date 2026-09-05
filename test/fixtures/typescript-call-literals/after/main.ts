export function run() {
    repo.change("lib.ts", './worker', `a.b::${suffix}`, /a\.b/, 1.25).unwrap();
    repo.state.change("lib.ts").result.finish();
    new (lookup("./worker")).Worker();
    "lib.ts".trim();
    (1.25).toFixed();
    workers["./worker"].run();
}
