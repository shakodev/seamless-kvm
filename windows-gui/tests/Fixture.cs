using System;
using System.Diagnostics;
using System.IO;
using System.Text;

internal static class Fixture {
    static int Main(string[] args) {
        switch (args[0]) {
            case "echo":
                for (int i = 1; i < args.Length; i++) Console.WriteLine(Convert.ToBase64String(Encoding.UTF8.GetBytes(args[i])));
                break;
            case "monitors": Console.WriteLine("left = 0  top = 0  width = 1920  height = 1080"); break;
            case "settings":
                Console.WriteLine("127.0.0.1:24800");
                Console.WriteLine("left = 0  top = 0  width = 1920  height = 1080");
                break;
            case "--managed":
                File.WriteAllText("host-pid.txt", Process.GetCurrentProcess().Id.ToString());
                Console.WriteLine("Host listening on 127.0.0.1:24800");
                Console.WriteLine("Authenticated client ready");
                Console.In.Read();
                break;
            case "check":
                if (File.Exists("fail-check")) { Console.Error.WriteLine("Check failed"); return 7; }
                Console.WriteLine("Check passed");
                break;
            case "build": case "clean":
                Console.Error.WriteLine("Fixture cargo " + args[0]);
                System.Threading.Thread.Sleep(150);
                break;
            case "configure": Console.WriteLine("Settings saved"); break;
            default: return 1;
        }
        return 0;
    }
}
