using System;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Reflection;
using System.Text;
using System.Threading;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Markup;
using System.Windows.Threading;

namespace SeamlessKVM {
    internal static class SmokeTests {
        static Window window;
        static Button Button(string name) { return (Button)window.FindName(name); }
        static string Status { get { return ((TextBlock)window.FindName("Status")).Text; } }
        static void Assert(bool condition, string message) { if (!condition) throw new Exception(message); }
        static void Pump(Func<bool> done) {
            var timer = Stopwatch.StartNew();
            while (!done()) {
                if (timer.ElapsedMilliseconds > 10000) throw new Exception("Timed out; status: " + Status);
                var frame = new DispatcherFrame();
                Dispatcher.CurrentDispatcher.BeginInvoke(DispatcherPriority.Background, new Action(() => frame.Continue = false));
                Dispatcher.PushFrame(frame);
                Thread.Sleep(5);
            }
        }
        static void Click(string name) {
            Assert(Button(name).IsEnabled, name + " should be enabled");
            Button(name).RaiseEvent(new RoutedEventArgs(System.Windows.Controls.Button.ClickEvent));
        }
        static bool Exited(int id) {
            try { using (var process = Process.GetProcessById(id)) return process.HasExited; }
            catch (ArgumentException) { return true; }
        }

        [STAThread]
        static int Main(string[] args) {
            try {
                SynchronizationContext.SetSynchronizationContext(new DispatcherSynchronizationContext(Dispatcher.CurrentDispatcher));
                Panel.HostProbe = () => false; // Never interact with the user's running host.
                string root = args[0], fixture = args[1];
                Directory.CreateDirectory(Path.Combine(root, "certs"));
                Directory.CreateDirectory(Path.Combine(root, "target", "gui-host", "release"));
                Directory.CreateDirectory(Path.Combine(root, ".tools", "cargo", "bin"));
                Directory.CreateDirectory(Path.Combine(root, ".tools", "rustup"));
                File.WriteAllText(Path.Combine(root, "certs", "host.local.toml"), "fixture");
                File.Copy(fixture, Path.Combine(root, "target", "gui-host", "release", "windows-host.exe"), true);
                File.Copy(fixture, Path.Combine(root, ".tools", "cargo", "bin", "cargo.exe"), true);
                string[] values = { "", "two words", "C:\\folder with spaces\\", "quote\"inside", "slash\\\"quote", "normal" };
                var info = new ProcessStartInfo(fixture, "echo " + String.Join(" ", values.Select(Panel.Quote))) { UseShellExecute = false, CreateNoWindow = true, RedirectStandardOutput = true };
                using (var process = Process.Start(info)) {
                    foreach (string value in values) Assert(process.StandardOutput.ReadLine() == Convert.ToBase64String(Encoding.UTF8.GetBytes(value)), "Argument round trip failed");
                    process.WaitForExit();
                }
                using (Stream source = Assembly.GetExecutingAssembly().GetManifestResourceStream("MainWindow.xaml")) window = (Window)XamlReader.Load(source);
                var panel = new Panel(window, root);
                window.Show();
                Pump(() => panel.Ready.IsCompleted);
                Assert(Status == "Stopped", "Initial status");
                Assert(Button("Start").IsEnabled, "Configured host can start");
                Panel.HostProbe = () => true;
                Pump(() => Status == "Already running");
                Assert(!Button("Start").IsEnabled && !Button("Check").IsEnabled, "External host blocks native startup");
                Panel.HostProbe = () => false;
                Pump(() => Status == "Stopped");
                ((ComboBox)window.FindName("Listen")).Text = "127.0.0.1:24801";
                Pump(() => !Button("Start").IsEnabled);
                Click("Save"); Pump(() => Button("Build").IsEnabled);
                Click("Start"); Pump(() => Status == "Connected");
                Assert(!Button("Browse").IsEnabled, "Configuration locked during session");
                Click("Start"); Pump(() => Status == "Stopped" && Button("Build").IsEnabled);
                Assert(Exited(Int32.Parse(File.ReadAllText(Path.Combine(root, "host-pid.txt")))), "Stop leaves no host");
                Click("Start"); Pump(() => Status == "Connected");
                Click("Rebuild"); Pump(() => Button("Build").IsEnabled);
                Assert(Status == "Stopped", "Rebuild stops server without restarting");
                Assert(Exited(Int32.Parse(File.ReadAllText(Path.Combine(root, "host-pid.txt")))), "Rebuild leaves no host");
                Click("Check"); Pump(() => Button("Build").IsEnabled);
                File.WriteAllText(Path.Combine(root, "fail-check"), "");
                Click("Check"); Pump(() => Button("Build").IsEnabled);
                Assert(Status == "Failed", "Nonzero exit is visible");
                Click("Build"); Pump(() => Button("Build").IsEnabled);
                Assert(Status == "Stopped", "Build recovers failed status");
                Click("Start"); Pump(() => Status == "Connected");
                int pid = Int32.Parse(File.ReadAllText(Path.Combine(root, "host-pid.txt")));
                window.Close(); Pump(() => !window.IsVisible);
                Assert(Exited(pid), "Closing window stops host");
                using (Stream source = Assembly.GetExecutingAssembly().GetManifestResourceStream("MainWindow.xaml")) window = (Window)XamlReader.Load(source);
                var second = new Panel(window, root);
                window.Show(); Pump(() => second.Ready.IsCompleted);
                Click("Build"); window.Close();
                Pump(() => !window.IsVisible);
                Assert(Status == "Stopped", "Closing during build waits for completion");
                Console.WriteLine("PASS: quoting, settings, external host, start/stop, rebuild, failures, recovery, close cleanup, close during build");
                return 0;
            } catch (Exception error) { Console.Error.WriteLine(error); return 1; }
        }
    }
}
