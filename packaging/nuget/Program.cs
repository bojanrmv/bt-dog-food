// Never run as the tool: the package's entry point is the native dgf binary
// (ToolEntryPoint in Dgf.csproj). A .NET tool is packed from a project, and
// this is that project's code.

Console.Error.WriteLine("dgf: this assembly is only the package's project; run the dgf command.");
return 3;
