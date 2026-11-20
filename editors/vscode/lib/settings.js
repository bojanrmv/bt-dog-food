// The extension's settings (`dogFood.*`).
"use strict";

const vscode = require("vscode");
const DEFAULTS = require("./defaults");

/* One setting. The default is given with every read: after an update installed
   while VS Code runs, VS Code can know no default for a setting that is new. */
const setting = name => vscode.workspace.getConfiguration("dogFood").get(name, DEFAULTS[name]);

/* The dgf executable: the `dogFood.path` setting, else `dgf` on the PATH. */
const program = () => setting("path") || DEFAULTS.path;

module.exports = { setting, program };
