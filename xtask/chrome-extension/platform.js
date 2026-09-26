// What a page reads about the platform, where Chrome's build says Linux:
// `navigator.platform`, and `navigator.userAgentData`'s platform in its
// low-entropy values, its `toJSON` and `getHighEntropyValues`. Chrome takes
// these from constants compiled in, not from `uname`, and no switch changes
// them. The user agent itself is `--user-agent`'s and the `Sec-CH-UA-Platform`
// header is rules.json's. A worker's navigator is not reached: content
// scripts do not run in workers.
(() => {
  const getter = (prototype, name, value) =>
    Object.defineProperty(prototype, name, { get: value, configurable: true, enumerable: true });
  getter(Navigator.prototype, "platform", () => "Ferrix x86_64");
  if (typeof NavigatorUAData === "undefined") {
    return;
  }
  const uaData = NavigatorUAData.prototype;
  const high = uaData.getHighEntropyValues;
  const json = uaData.toJSON;
  getter(uaData, "platform", () => "Ferrix");
  uaData.getHighEntropyValues = function (hints) {
    return high.call(this, hints).then((values) => ({ ...values, platform: "Ferrix" }));
  };
  uaData.toJSON = function () {
    return { ...json.call(this), platform: "Ferrix" };
  };
})();
