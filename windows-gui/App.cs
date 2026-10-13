using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Net.NetworkInformation;
using System.Net.Sockets;
using System.Reflection;
using System.Text;
using System.Text.RegularExpressions;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Markup;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using Microsoft.Win32;

namespace SeamlessKVM {
    internal sealed class Display {
        internal string Raw;
        internal string[] Bounds;
        public override string ToString() {
            return Bounds[2] + " × " + Bounds[3] + "  (" + Bounds[0] + ", " + Bounds[1] + ")";
        }
        internal static Display Parse(string text) {
            Match m = Regex.Match(text, @"^left = (-?\d+)  top = (-?\d+)  width = (\d+)  height = (\d+)$");
            if (!m.Success) return null;
            return new Display { Raw = text, Bounds = m.Groups.Cast<Group>().Skip(1).Select(g => g.Value).ToArray() };
        }
    }

    internal sealed class Panel {
        readonly Window window;
        readonly string root, binary, preference;
        readonly Queue<string> lines = new Queue<string>();
        Process host;
        string config;
        bool busy, stopping, loading, dirty, closeRequested;
        bool externalHost;
        readonly TaskCompletionSource<bool> ready = new TaskCompletionSource<bool>();
        internal Task Ready { get { return ready.Task; } }

        T Find<T>(string name) where T : FrameworkElement { return (T)window.FindName(name); }
        Button Button(string name) { return Find<Button>(name); }
        TextBlock Text(string name) { return Find<TextBlock>(name); }
        ComboBox Listen { get { return Find<ComboBox>("Listen"); } }
        ComboBox Monitor { get { return Find<ComboBox>("Monitor"); } }

        internal Panel(Window window, string root) {
            this.window = window;
            this.root = root;
            externalHost = OtherHostRunning();
            var statusTimer = new System.Windows.Threading.DispatcherTimer { Interval = TimeSpan.FromSeconds(2) };
            statusTimer.Tick += (s, e) => {
                bool running = OtherHostRunning();
                if (running != externalHost) { externalHost = running; Refresh(); }
            };
            statusTimer.Start();
            window.Closed += (s, e) => statusTimer.Stop();
            binary = Path.Combine(root, "target", "gui-host", "release", "windows-host.exe");
            preference = Path.Combine(root, "target", "gui", "configuration.txt");
            config = Path.Combine(root, "certs", "host.local.toml");
            if (File.Exists(preference)) {
                string saved = File.ReadAllText(preference).Trim();
                if (File.Exists(saved)) config = saved;
            }
            Button("Start").Click += async (s, e) => await Action(async () => {
                if (host != null) await Stop(); else Start();
            });
            Button("Build").Click += async (s, e) => await Action(() => Build(false));
            Button("Rebuild").Click += async (s, e) => await Action(() => Build(true));
            Button("Browse").Click += async (s, e) => await Action(async () => {
                var dialog = new OpenFileDialog { Filter = "Host configuration (*.toml)|*.toml", InitialDirectory = root };
                if (dialog.ShowDialog(window) != true) return;
                config = dialog.FileName;
                Remember();
                await LoadSettings();
            });
            Button("Pair").Click += async (s, e) => await Action(async () => {
                var dialog = new SaveFileDialog { Title = "Create pairing in a new folder", Filter = "Host configuration|host.local.toml", FileName = "host.local.toml", InitialDirectory = root };
                if (dialog.ShowDialog(window) != true) return;
                string directory = Path.GetDirectoryName(dialog.FileName);
                await Run(binary, new[] { "pair", directory }, true);
                config = Path.Combine(directory, "host.local.toml");
                Remember();
                await LoadSettings();
            });
            Button("Save").Click += async (s, e) => await Action(async () => {
                Display display = Monitor.SelectedItem as Display;
                if (display == null) throw new InvalidOperationException("Select a Windows display.");
                await Run(binary, new[] { "configure", config, Listen.Text.Trim() }.Concat(display.Bounds).ToArray(), true);
                dirty = false;
                await LoadSettings();
            });
            Button("Check").Click += async (s, e) => await Action(async () => {
                await Run(binary, new[] { "check", config }, true);
                Log("Check passed.");
            });
            Button("Clear").Click += (s, e) => { lines.Clear(); Find<TextBox>("Activity").Clear(); };
            Listen.AddHandler(TextBox.TextChangedEvent, new TextChangedEventHandler((s, e) => Changed()));
            Monitor.SelectionChanged += (s, e) => Changed();
            window.Closing += async (s, e) => {
                if (host == null && !busy) return;
                e.Cancel = true;
                closeRequested = true;
                if (busy) { Log("Closing after the current operation finishes."); return; }
                await Action(Stop);
            };
            window.Loaded += async (s, e) => { await Action(LoadSettings); ready.TrySetResult(true); };
            Refresh();
        }

        void Changed() {
            if (loading || busy) return;
            dirty = true;
            Refresh();
        }

        void Remember() {
            Directory.CreateDirectory(Path.GetDirectoryName(preference));
            File.WriteAllText(preference, config);
        }

        void Log(string line) {
            if (String.IsNullOrWhiteSpace(line)) return;
            // Bound both individual messages and the number of displayed lines.
            if (line.Length > 1500) line = line.Substring(0, 1500);
            lines.Enqueue(DateTime.Now.ToString("HH:mm:ss", CultureInfo.InvariantCulture) + "  " + line);
            while (lines.Count > 180) lines.Dequeue();
            var activity = Find<TextBox>("Activity");
            activity.Text = String.Join(Environment.NewLine, lines);
            activity.ScrollToEnd();
        }

        void Status(string label, string detail, string color) {
            Text("Status").Text = label;
            Text("StatusDetail").Text = detail;
            Find<System.Windows.Shapes.Ellipse>("StatusDot").Fill = new SolidColorBrush((Color)ColorConverter.ConvertFromString(color));
        }

        void Refresh() {
            bool built = File.Exists(binary), configured = File.Exists(config), running = host != null;
            Find<TextBox>("ConfigPath").Text = configured ? config : "No configuration selected";
            Find<TextBox>("ConfigPath").ToolTip = configured ? config : null;
            Text("BuildDetail").Text = built ? "Built " + File.GetLastWriteTime(binary).ToString("dd MMM yyyy, HH:mm", CultureInfo.InvariantCulture) : "Not built";
            Button("Start").Content = running ? "S_top server" : "_Start server";
            Button("Start").IsEnabled = !busy && (running || (built && configured && !dirty && !externalHost));
            Button("Build").IsEnabled = Button("Rebuild").IsEnabled = !busy;
            Button("Browse").IsEnabled = !busy && !running;
            Button("Pair").IsEnabled = !busy && !running && built;
            Button("Check").IsEnabled = !busy && !running && !externalHost && built && configured && !dirty;
            Find<Grid>("Settings").IsEnabled = !busy && !running && built && configured;
            Button("Save").IsEnabled = dirty;
            if (!running && !busy && externalHost)
                Status("Already running", "Stop the existing host before starting here", "#A46C32");
            else if (!running && !busy && Text("Status").Text != "Failed")
                Status("Stopped", dirty ? "Save settings before starting" : !built ? "Build the host to get started" : !configured ? "Select a configuration or create pairing" : "Ready to start", "#88928B");
        }

        internal static Func<bool> HostProbe = () => {
            try { using (System.Threading.Mutex.OpenExisting(@"Local\SeamlessKVMHost")) return true; }
            catch (System.Threading.WaitHandleCannotBeOpenedException) { return false; }
            catch (UnauthorizedAccessException) { return true; }
        };
        bool OtherHostRunning() { return host == null && HostProbe(); }

        async Task Action(Func<Task> work) {
            if (busy) return;
            busy = true;
            Refresh();
            try { await work(); }
            catch (Exception error) { closeRequested = false; Log(error.Message); if (host == null) Status("Failed", "See activity for details", "#A46C32"); }
            finally {
                busy = false;
                Refresh();
            }
            if (closeRequested) {
                if (host != null) await Action(Stop);
                else window.Close();
            }
        }

        internal static string Quote(string value) {
            // CommandLineToArgvW quoting, including a directory's trailing slash.
            var result = new StringBuilder("\"");
            int slashes = 0;
            foreach (char character in value) {
                if (character == '\\') { slashes++; continue; }
                result.Append('\\', character == '"' ? slashes * 2 + 1 : slashes);
                result.Append(character);
                slashes = 0;
            }
            return result.Append('\\', slashes * 2).Append('"').ToString();
        }

        Process ProcessFor(string file, string[] args) {
            var info = new ProcessStartInfo(file, String.Join(" ", args.Select(Quote))) {
                WorkingDirectory = root, UseShellExecute = false, CreateNoWindow = true,
                RedirectStandardOutput = true, RedirectStandardError = true, RedirectStandardInput = true,
                StandardOutputEncoding = Encoding.UTF8, StandardErrorEncoding = Encoding.UTF8
            };
            string localCargo = Path.Combine(root, ".tools", "cargo");
            string localRustup = Path.Combine(root, ".tools", "rustup");
            if (Directory.Exists(localRustup) && File.Exists(Path.Combine(localCargo, "bin", "cargo.exe"))) {
                info.EnvironmentVariables["CARGO_HOME"] = localCargo;
                info.EnvironmentVariables["RUSTUP_HOME"] = localRustup;
                info.EnvironmentVariables["PATH"] = Path.Combine(localCargo, "bin") + ";" + Path.Combine(root, ".tools", "w64devkit", "bin") + ";" + info.EnvironmentVariables["PATH"];
            }
            info.EnvironmentVariables["CARGO_TERM_COLOR"] = "never";
            return new Process { StartInfo = info };
        }

        void Dispatch(Action action) {
            if (!window.Dispatcher.HasShutdownStarted) window.Dispatcher.BeginInvoke(action);
        }

        async Task<string[]> Run(string file, string[] args, bool log) {
            using (Process process = ProcessFor(file, args)) {
                var output = new List<string>();
                process.OutputDataReceived += (s, e) => {
                    if (e.Data == null) return;
                    if (!log) { lock (output) { output.Add(e.Data); } }
                    if (log) Dispatch(() => Log(e.Data));
                };
                process.ErrorDataReceived += (s, e) => { if (e.Data != null) Dispatch(() => Log(e.Data)); };
                process.Start();
                process.StandardInput.Close();
                process.BeginOutputReadLine();
                process.BeginErrorReadLine();
                await Task.Run(() => process.WaitForExit());
                if (process.ExitCode != 0) throw new InvalidOperationException(Path.GetFileNameWithoutExtension(file) + " exited with code " + process.ExitCode + ".");
                lock (output) { return output.ToArray(); }
            }
        }

        async Task LoadSettings() {
            loading = true;
            try {
                Listen.Items.Clear();
                Listen.Items.Add("127.0.0.1:24800");
                foreach (var adapter in NetworkInterface.GetAllNetworkInterfaces().Where(a => a.OperationalStatus == OperationalStatus.Up))
                    foreach (var address in adapter.GetIPProperties().UnicastAddresses.Where(a => a.Address.AddressFamily == AddressFamily.InterNetwork)) {
                        string value = address.Address + ":24800";
                        if (!Listen.Items.Contains(value)) Listen.Items.Add(value);
                    }
                Monitor.Items.Clear();
                if (!File.Exists(binary)) return;
                foreach (string line in await Run(binary, new[] { "monitors" }, false)) {
                    Display display = Display.Parse(line);
                    if (display != null) Monitor.Items.Add(display);
                }
                if (!File.Exists(config)) return;
                string[] settings = await Run(binary, new[] { "settings", config }, false);
                if (settings.Length != 2) throw new InvalidOperationException("Could not read host settings.");
                Listen.Text = settings[0];
                Display selected = Monitor.Items.Cast<Display>().FirstOrDefault(d => d.Raw == settings[1]);
                if (selected != null) Monitor.SelectedItem = selected;
                else { Monitor.SelectedIndex = 0; Log("Configured display is unavailable. Select a display and save."); }
                dirty = selected == null;
            } finally { loading = false; }
        }

        async Task Build(bool clean) {
            await Stop();
            Status(clean ? "Rebuilding" : "Building", "Release · Windows host", "#A46C32");
            string localCargo = Path.Combine(root, ".tools", "cargo", "bin", "cargo.exe");
            string cargo = File.Exists(localCargo) ? localCargo : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".cargo", "bin", "cargo.exe");
            if (!File.Exists(cargo)) cargo = "cargo.exe";
            if (clean) {
                Log("Cleaning host build…");
                await Run(cargo, new[] { "clean", "--release", "-p", "windows-host", "--target-dir", "target/gui-host" }, true);
            }
            Log("Building host…");
            await Run(cargo, new[] { "build", "--release", "-p", "windows-host", "--target-dir", "target/gui-host" }, true);
            Log("Build complete.");
            Status("Stopped", "Ready to start", "#88928B");
            await LoadSettings();
        }

        void Start() {
            var process = ProcessFor(binary, new[] { "--managed", config });
            DataReceivedEventHandler receive = (s, e) => {
                if (e.Data == null) return;
                Dispatch(() => {
                    Log(e.Data);
                    if (host != process || stopping) return;
                    if (e.Data.StartsWith("Host listening on ")) Status("Listening", Listen.Text, "#396B57");
                    else if (e.Data == "Authenticated client ready") Status("Connected", "Client authenticated", "#396B57");
                    else if (e.Data == "Session ended; local control restored") Status("Listening", "Client disconnected · local control restored", "#396B57");
                });
            };
            process.OutputDataReceived += receive;
            process.ErrorDataReceived += receive;
            try { process.Start(); }
            catch { process.Dispose(); throw; }
            host = process;
            process.BeginOutputReadLine();
            process.BeginErrorReadLine();
            Status("Starting", "Checking configuration and input", "#A46C32");
            WatchHost(process);
        }

        async void WatchHost(Process process) {
            try {
                await Task.Run(() => process.WaitForExit());
                if (host != process || stopping) return;
                int code = process.ExitCode;
                host = null;
                process.Dispose();
                Log("Server exited with code " + code + ".");
                Status(code == 0 ? "Stopped" : "Failed", code == 0 ? "Local control restored" : "See activity for details", code == 0 ? "#88928B" : "#A46C32");
                Refresh();
            } catch (InvalidOperationException) { /* Stop owns disposal after exit. */ }
        }

        async Task Stop() {
            if (host == null) return;
            Process process = host;
            stopping = true;
            Status("Stopping", "Restoring local control", "#A46C32");
            try {
                // Closing our exclusive write end also covers parent crashes.
                process.StandardInput.Close();
                bool exited = await Task.Run(() => process.WaitForExit(5000));
                if (!exited) {
                    Log("Server did not stop in time; ending it. The input watchdog restores the cursor.");
                    process.Kill();
                    await Task.Run(() => process.WaitForExit());
                }
                Log("Server stopped.");
                host = null;
                process.Dispose();
                Status("Stopped", "Local control restored", "#88928B");
            } finally { stopping = false; }
        }
    }

    internal static class Program {
        [STAThread]
        static void Main(string[] args) {
            try {
                string root = AppDomain.CurrentDomain.BaseDirectory;
                while (!File.Exists(Path.Combine(root, "Cargo.toml")) || !Directory.Exists(Path.Combine(root, "windows-host"))) {
                    var parent = Directory.GetParent(root.TrimEnd(Path.DirectorySeparatorChar));
                    if (parent == null) throw new InvalidOperationException("Keep the GUI inside the SeamlessKVM repository.");
                    root = parent.FullName;
                }
                var app = new Application();
                Window window;
                using (Stream source = Assembly.GetExecutingAssembly().GetManifestResourceStream("MainWindow.xaml"))
                    window = (Window)XamlReader.Load(source);
                var panel = new Panel(window, root);
                if (args.Length == 2 && args[0] == "--preview") {
                    window.Loaded += async (s, e) => {
                        await panel.Ready;
                        window.UpdateLayout();
                        var surface = (FrameworkElement)VisualTreeHelper.GetChild(window, 0);
                        var bitmap = new RenderTargetBitmap((int)surface.ActualWidth, (int)surface.ActualHeight, 96, 96, PixelFormats.Pbgra32);
                        bitmap.Render(surface);
                        var png = new PngBitmapEncoder();
                        png.Frames.Add(BitmapFrame.Create(bitmap));
                        using (var output = File.Create(args[1])) png.Save(output);
                        window.Close();
                    };
                }
                app.Run(window);
            } catch (Exception error) {
                MessageBox.Show(error.Message, "SeamlessKVM", MessageBoxButton.OK, MessageBoxImage.Error);
                Environment.ExitCode = 1;
            }
        }
    }
}
