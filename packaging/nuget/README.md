# dog-food (`dgf`)

Measures how hard code is to change and how much it is at risk: complexity and CRAP scores per function, duplicated code with how to unite it, and mutation testing that shows what the tests do not catch. For C#, TypeScript, JavaScript and Rust.

```powershell
dotnet tool install -g dgf
dgf analyze                  # complexity and CRAP, worst functions first
dgf dup --format html -o dup.html   # duplicated code, reviewed in the browser
dgf mutate src/Orders.cs --run      # what the tests let through
```

The work is done by one native binary (written in Rust) for your platform; this package carries it, so it runs at full speed and needs only the .NET 10 runtime to start. The C# duplication engine (`dgf dup --semantic`, built on Roslyn) is included.

Documentation: https://github.com/bojanrmv/bt-dog-food
