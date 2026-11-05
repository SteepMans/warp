# Ward: the tab of an agent session that runs on another machine (WARD.md, "Подключиться").
#
# Ward writes this file and runs it in a tab of its own with WARD_ATTACH_URL (the viewer socket
# of the agents service with a one-time ticket), WARD_ATTACH_TITLE (the task) and
# WARD_ATTACH_WORKER (the machine of the session). The viewer connects to the socket, prints the
# output of the session's tab and sends the keys typed here back as binary frames, as the
# viewer in the browser does. Ctrl+] disconnects; the session on its own machine goes on.
#
# The output is the raw output of a terminal on another machine, and that terminal already
# answered what the session asked of it. What would make this terminal answer again (the answer
# would go to the session as typed keys), or take this tab for a shell or an agent of its own,
# is left out (see Filter): queries, the marks of Warp's shell and agent integration, titles.
#
# The raw output fits a screen of its own size only. So the viewer sends the size of this tab
# ({"type":"size","cols","rows"}) when it connects and whenever the tab is resized: the session's
# tab takes it, and its machine redraws the screen at that size. Until then the output is held,
# not drawn (see Screen): what was drawn for another size is dropped, the redraw is shown. The
# Ward of the session introduces itself first ({"type":"hello","features":["size","redraw"]});
# one that does not is older, may never take the size and gets the old behavior after a few
# seconds: what came is drawn as it is, and the viewer says the screen may come out doubled.
#
# Where the stream is goes to the title of this console: "Ward: <state>", "Ward! <warning>". The
# header of the tab shows it (remote_tab.rs), since the output of the session draws over
# anything printed here.

$ErrorActionPreference = 'Stop'
$url = $env:WARD_ATTACH_URL
$title = [string]$env:WARD_ATTACH_TITLE
$worker = [string]$env:WARD_ATTACH_WORKER
Remove-Item Env:\WARD_ATTACH_URL -ErrorAction SilentlyContinue
if (-not $url) {
    Write-Host 'Ward: нет адреса трансляции. Откройте сессию кнопкой «Подключиться» в карточке.'
    exit 2
}

# Windows PowerShell may still offer only old TLS versions to the service.
if ($PSVersionTable.PSEdition -ne 'Core') {
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    } catch { }
}

$source = @'
using System;
using System.Collections.Generic;
using System.IO;
using System.Net.WebSockets;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.RegularExpressions;
using System.Threading;
using System.Threading.Tasks;

namespace WardAttach
{
    /// The output of the session's tab as this terminal may be given it. The terminal of the
    /// session's machine was given the same output and answered its queries already; asked again
    /// here, this one would answer too, and the answer would go to the session as typed keys. So
    /// what draws the screen goes on (text, colors, cursor moves, the alternate screen, links,
    /// bracketed paste, focus and mouse reports), and the rest is left out: queries and reports,
    /// the marks of Warp's shell and agent integration and every other OSC but links, DCS, APC,
    /// PM and SOS strings, the keyboard and window modes of the other terminal, its title.
    /// Sequences cut between two pieces of output are put together first.
    public sealed class Filter
    {
        enum Mode { Ground, Escape, Csi, Osc, OscEscape, Drop, DropEscape }

        const char Esc = '\u001b';
        const char Bel = '\u0007';
        const int MaxSequence = 4096;
        const int MaxDropped = 1024 * 1024;
        // DEC private modes of the other terminal that are not this tab's to take: win32 input
        // (9001), color scheme reports (2031), in-band resize reports (2048), mouse in pixels (1016).
        static readonly HashSet<string> DroppedModes = new HashSet<string> { "9001", "2031", "2048", "1016" };

        Mode mode = Mode.Ground;
        readonly StringBuilder sequence = new StringBuilder();
        int dropped;

        public void Reset()
        {
            mode = Mode.Ground;
            sequence.Clear();
            dropped = 0;
        }

        public string Feed(string text)
        {
            StringBuilder kept = new StringBuilder(text.Length);
            int i = 0;
            while (i < text.Length)
            {
                char c = text[i];
                switch (mode)
                {
                    case Mode.Ground:
                        if (c == Esc)
                        {
                            Begin(Mode.Escape, c);
                        }
                        else
                        {
                            kept.Append(c);
                        }
                        break;
                    case Mode.Escape:
                        if (c == '[')
                        {
                            mode = Mode.Csi;
                            sequence.Append(c);
                        }
                        else if (c == ']')
                        {
                            mode = Mode.Osc;
                            sequence.Append(c);
                        }
                        else if (c == 'P' || c == '_' || c == '^' || c == 'X')
                        {
                            // DCS, APC, PM, SOS.
                            mode = Mode.Drop;
                            dropped = 0;
                        }
                        else if (c == Esc)
                        {
                            Begin(Mode.Escape, c);
                        }
                        else if (c == 'Z')
                        {
                            // DECID asks the terminal who it is.
                            mode = Mode.Ground;
                        }
                        else if (c == '\u0018' || c == '\u001a')
                        {
                            mode = Mode.Ground;
                        }
                        else
                        {
                            kept.Append(Esc).Append(c);
                            mode = Mode.Ground;
                        }
                        break;
                    case Mode.Csi:
                        if (c == Esc)
                        {
                            Begin(Mode.Escape, c);
                        }
                        else if (c == '\u0018' || c == '\u001a')
                        {
                            mode = Mode.Ground;
                        }
                        else if (c < ' ')
                        {
                            // A control inside a sequence acts at once.
                            kept.Append(c);
                        }
                        else
                        {
                            sequence.Append(c);
                            if (c >= '@' && c <= '~')
                            {
                                kept.Append(Csi(sequence.ToString()));
                                mode = Mode.Ground;
                            }
                            else if (sequence.Length > MaxSequence)
                            {
                                mode = Mode.Ground;
                            }
                        }
                        break;
                    case Mode.Osc:
                        if (c == Bel)
                        {
                            kept.Append(Osc(sequence.ToString(), "\u0007"));
                            mode = Mode.Ground;
                        }
                        else if (c == Esc)
                        {
                            mode = Mode.OscEscape;
                        }
                        else if (c == '\u0018' || c == '\u001a')
                        {
                            mode = Mode.Ground;
                        }
                        else
                        {
                            sequence.Append(c);
                            if (sequence.Length > MaxSequence)
                            {
                                mode = Mode.Drop;
                                dropped = 0;
                            }
                        }
                        break;
                    case Mode.OscEscape:
                        if (c == '\\')
                        {
                            kept.Append(Osc(sequence.ToString(), "\u001b\\"));
                            mode = Mode.Ground;
                            break;
                        }
                        // An escape ends the string: what follows is a new sequence.
                        Begin(Mode.Escape, Esc);
                        continue;
                    case Mode.Drop:
                        if (c == Esc)
                        {
                            mode = Mode.DropEscape;
                        }
                        else if (c == Bel || c == '\u0018' || c == '\u001a' || ++dropped > MaxDropped)
                        {
                            mode = Mode.Ground;
                        }
                        break;
                    case Mode.DropEscape:
                        if (c == '\\')
                        {
                            mode = Mode.Ground;
                            break;
                        }
                        Begin(Mode.Escape, Esc);
                        continue;
                }
                i++;
            }
            return kept.ToString();
        }

        void Begin(Mode next, char first)
        {
            mode = next;
            sequence.Clear();
            sequence.Append(first);
        }

        /// A whole CSI sequence: itself, the part of it to keep, or nothing.
        static string Csi(string whole)
        {
            char final = whole[whole.Length - 1];
            string body = whole.Substring(2, whole.Length - 3);
            char prefix = body.Length > 0 && "<=>?".IndexOf(body[0]) >= 0 ? body[0] : '\0';
            string rest = prefix == '\0' ? body : body.Substring(1);
            int end = rest.Length;
            while (end > 0 && rest[end - 1] >= ' ' && rest[end - 1] <= '/')
            {
                end--;
            }
            string parameters = rest.Substring(0, end);
            string intermediates = rest.Substring(end);
            bool plain = intermediates.Length == 0;
            switch (final)
            {
                case 'c':
                    // DA1, DA2, DA3.
                    return "";
                case 'n':
                    // Status and cursor position reports.
                    return "";
                case 't':
                    // Window operations: reports, resizing, the title stack.
                    return plain ? "" : whole;
                case 'u':
                    // The kitty keyboard protocol; a plain one restores the cursor.
                    return prefix != '\0' ? "" : whole;
                case 'm':
                    // Key modifier modes and their queries; a plain one is colors.
                    return prefix != '\0' ? "" : whole;
                case 'q':
                    // XTVERSION.
                    return prefix == '>' ? "" : whole;
                case 'x':
                    // DECREQTPARM.
                    return plain ? "" : whole;
                case 'p':
                case 'w':
                    // DECRQM, DECRQPSR.
                    return intermediates.IndexOf('$') >= 0 ? "" : whole;
                case '|':
                    // DECSCPP changes the width of the window.
                    return intermediates.IndexOf('$') >= 0 ? "" : whole;
                case 'y':
                    // DECRQCRA.
                    return intermediates.IndexOf('*') >= 0 ? "" : whole;
                case 'S':
                    // XTSMGRAPHICS.
                    return prefix == '?' ? "" : whole;
                case 'h':
                case 'l':
                    if (prefix != '?' || !plain)
                    {
                        return whole;
                    }
                    List<string> modes = new List<string>();
                    foreach (string m in parameters.Split(';'))
                    {
                        if (!DroppedModes.Contains(m))
                        {
                            modes.Add(m);
                        }
                    }
                    if (modes.Count == 0)
                    {
                        return "";
                    }
                    return "\u001b[?" + string.Join(";", modes.ToArray()) + final;
                default:
                    return whole;
            }
        }

        /// A whole OSC string: links stay, the rest is the other terminal's.
        static string Osc(string body, string terminator)
        {
            // body starts with "\e]".
            if (body.StartsWith("\u001b]8;", StringComparison.Ordinal))
            {
                return body + terminator;
            }
            return "";
        }
    }

    /// The JSON control frames of the service, read without a JSON library.
    public static class Frames
    {
        public static string Field(string json, string name)
        {
            Match match = Regex.Match(json, "\"" + name + "\"\\s*:\\s*\"((?:[^\"\\\\]|\\\\.)*)\"");
            if (!match.Success)
            {
                return null;
            }
            try
            {
                return Regex.Unescape(match.Groups[1].Value);
            }
            catch
            {
                return match.Groups[1].Value;
            }
        }

        public static int Number(string json, string name)
        {
            Match match = Regex.Match(json, "\"" + name + "\"\\s*:\\s*(\\d+)");
            int value;
            return match.Success && int.TryParse(match.Groups[1].Value, out value) ? value : 0;
        }

        /// Whether `{"type":"hello","features":[…]}` names the feature.
        public static bool Has(string json, string feature)
        {
            Match list = Regex.Match(json, "\"features\"\\s*:\\s*\\[([^\\]]*)\\]");
            if (!list.Success)
            {
                return false;
            }
            foreach (Match item in Regex.Matches(list.Groups[1].Value, "\"((?:[^\"\\\\]|\\\\.)*)\""))
            {
                if (item.Groups[1].Value == feature)
                {
                    return true;
                }
            }
            return false;
        }
    }

    /// What the viewer does with its screen after a frame.
    public sealed class Step
    {
        /// Wipe the screen first: what is on it was drawn for another size.
        public bool Clear;
        /// The session's tab kept another size: tell the owner if this tab is smaller.
        public bool Warn;
        /// Output of the session's tab to draw, in order.
        public readonly List<byte[]> Draw = new List<byte[]>();
    }

    /// The screen of the session in this tab. The output of the session's tab is held, not
    /// drawn, until that tab has the size of this one. Output that came while it had another
    /// size is dropped and the screen wiped: the session's machine redraws after the change.
    ///
    /// The Ward of the session says in its hello what it does: takes the size of this tab
    /// ("size"), and draws the screen again even at a size its tab has already ("redraw"; it
    /// takes another size for a moment and then this one). With it, this tab holds the output
    /// whenever either size changes, and at the start waits for that redraw rather than drawing
    /// the recent output, which may begin in the middle of the screen. A Ward that said nothing
    /// before its screen is older: when the sizes do not meet within HoldFor, what was held is
    /// drawn as it is. Nor does a newer one get forever: past RedrawFor the same happens.
    public sealed class Screen
    {
        public static readonly TimeSpan HoldFor = TimeSpan.FromSeconds(3);
        public static readonly TimeSpan RedrawFor = TimeSpan.FromSeconds(5);
        const int HeldCap = 4 * 1024 * 1024;

        readonly List<byte[]> held = new List<byte[]>();
        int heldBytes;
        int ownCols;
        int ownRows;
        int remoteCols;
        int remoteRows;
        bool holding;
        // What is held, or on the screen, was drawn for another size.
        bool foreign;
        bool sized;
        bool introduced;
        bool follows;
        bool redraws;
        bool heard;
        DateTime deadline = DateTime.MaxValue;

        /// Without the size of this tab there is nothing to wait for: output is drawn at once.
        public Screen(int ownCols, int ownRows)
        {
            this.ownCols = ownCols;
            this.ownRows = ownRows;
            Reset();
        }

        public bool Holding { get { return holding; } }
        public int RemoteCols { get { return remoteCols; } }
        public int RemoteRows { get { return remoteRows; } }
        /// A size or output of the session's tab came since the screen started.
        public bool Heard { get { return heard; } }
        /// The Ward of the session introduced itself since the screen started.
        public bool Introduced { get { return introduced; } }
        /// The Ward of the session is older: its screen came without a hello.
        public bool Legacy { get { return heard && !introduced; } }

        bool KnowsOwnSize { get { return ownCols > 0 && ownRows > 0; } }

        TimeSpan Wait { get { return follows ? RedrawFor : HoldFor; } }

        /// The session's machine starts its screen over (it connected again).
        public void Reset()
        {
            held.Clear();
            heldBytes = 0;
            holding = KnowsOwnSize;
            foreign = false;
            sized = false;
            introduced = false;
            follows = false;
            redraws = false;
            heard = false;
            remoteCols = 0;
            remoteRows = 0;
            deadline = DateTime.MaxValue;
        }

        /// The hello of the Ward of the session: whether its tab takes the size of this one and
        /// draws its screen again at a size it has.
        public void Hello(bool takesSize, bool redrawsScreen)
        {
            introduced = true;
            follows = takesSize;
            redraws = takesSize && redrawsScreen;
        }

        /// A piece of output of the session's tab.
        public Step Output(byte[] chunk, DateTime now)
        {
            heard = true;
            Step step = new Step();
            if (!holding)
            {
                step.Draw.Add(chunk);
                return step;
            }
            // Output before any size may be of any size.
            if (!sized)
            {
                foreign = true;
            }
            held.Add(chunk);
            heldBytes += chunk.Length;
            while (heldBytes > HeldCap && held.Count > 1)
            {
                heldBytes -= held[0].Length;
                held.RemoveAt(0);
            }
            StartWaiting(now);
            return step;
        }

        /// The size of the session's tab.
        public Step Size(int cols, int rows, DateTime now)
        {
            heard = true;
            Step step = new Step();
            remoteCols = cols;
            remoteRows = rows;
            sized = true;
            if (!KnowsOwnSize)
            {
                return step;
            }
            bool mine = cols == ownCols && rows == ownRows;
            if (!holding)
            {
                // The session's tab took another size (for another viewer, or for a redraw):
                // what comes now is drawn for that one.
                if (!mine && follows)
                {
                    Hold(now);
                }
                return step;
            }
            if (!mine)
            {
                foreign = true;
                StartWaiting(now);
                return step;
            }
            if (!foreign && redraws)
            {
                // The sizes meet already, and the session's machine draws the screen again for
                // this tab: that redraw is the screen, not the recent output.
                StartWaiting(now);
                return step;
            }
            return Release(step, foreign);
        }

        /// This tab was resized. The session's tab follows it and draws again; until then what
        /// comes is for the former size. An older Ward may not follow: its output goes on.
        public void Own(int cols, int rows, DateTime now)
        {
            if (cols == ownCols && rows == ownRows)
            {
                return;
            }
            ownCols = cols;
            ownRows = rows;
            if (follows && KnowsOwnSize)
            {
                Hold(now);
            }
        }

        /// How long the next frame may be waited for before Tick; infinite while not holding.
        public TimeSpan Left(DateTime now)
        {
            if (!holding || deadline == DateTime.MaxValue)
            {
                return Timeout.InfiniteTimeSpan;
            }
            TimeSpan left = deadline - now;
            return left > TimeSpan.Zero ? left : TimeSpan.Zero;
        }

        /// Time went by: past the deadline the held output is drawn as it is.
        public Step Tick(DateTime now)
        {
            Step step = new Step();
            if (!holding || now < deadline)
            {
                return step;
            }
            step.Warn = sized && (remoteCols != ownCols || remoteRows != ownRows);
            return Release(step, false);
        }

        void Hold(DateTime now)
        {
            holding = true;
            foreign = true;
            deadline = now + Wait;
        }

        Step Release(Step step, bool clear)
        {
            holding = false;
            deadline = DateTime.MaxValue;
            if (clear)
            {
                step.Clear = true;
            }
            else
            {
                step.Draw.AddRange(held);
            }
            held.Clear();
            heldBytes = 0;
            foreign = false;
            return step;
        }

        void StartWaiting(DateTime now)
        {
            if (deadline == DateTime.MaxValue)
            {
                deadline = now + Wait;
            }
        }
    }

    /// Where the stream of the session is.
    public enum Link
    {
        /// The socket of the service is being opened, or the Ward of the session connected again.
        Connecting,
        /// The service waits for the Ward of the session: the session is starting.
        Waiting,
        /// The Ward of the session streams.
        Live,
        /// An older Ward streams: it may never take the size of this tab.
        Legacy,
        /// The Ward of the session went away; the service is asked for it again.
        Reconnecting,
        /// The stream is over.
        Ended,
    }

    /// What the viewer says of the stream, in this tab and in its title. The header of the tab
    /// reads the title: "Ward: <state>", and "Ward! <state>" for a warning (remote_tab.rs).
    public static class Status
    {
        public const string Prefix = "Ward: ";
        public const string WarningPrefix = "Ward! ";

        public static string Text(Link link, string worker)
        {
            string machine = string.IsNullOrEmpty(worker) ? "машине сессии" : worker;
            switch (link)
            {
                case Link.Connecting:
                    return "подключаемся к " + machine;
                case Link.Waiting:
                    return "ждём Ward на " + machine + " (сессия запускается, агент ещё не стартовал)";
                case Link.Live:
                    return "подключено";
                case Link.Legacy:
                    return "на " + machine + " Ward старой версии: экран может задваиваться, обновите Ward там";
                case Link.Reconnecting:
                    return "Ward на " + machine + " отключился, переподключаемся";
                default:
                    return "трансляция закончилась";
            }
        }

        public static bool Warning(Link link)
        {
            return link == Link.Legacy || link == Link.Reconnecting;
        }

        public static string Title(Link link, string worker)
        {
            return (Warning(link) ? WarningPrefix : Prefix) + Text(link, worker);
        }
    }

    public static class Viewer
    {
        const int StdInput = -10;
        const int StdOutput = -11;
        const uint ProcessedInput = 0x0001;
        const uint LineInput = 0x0002;
        const uint EchoInput = 0x0004;
        const uint WindowInput = 0x0008;
        const uint MouseInput = 0x0010;
        const uint QuickEdit = 0x0040;
        const uint ExtendedFlags = 0x0080;
        const uint VirtualTerminalInput = 0x0200;
        const uint ProcessedOutput = 0x0001;
        const uint VirtualTerminalProcessing = 0x0004;
        const uint NoAutoReturn = 0x0008;
        // Ctrl+]
        const char Detach = '\u001d';
        // What a TUI of the session may have left switched on: the alternate screen, mouse
        // reports, bracketed paste, focus events, a hidden cursor, colors.
        const string Restore = "\u001b[?1049l\u001b[?1000l\u001b[?1002l\u001b[?1003l\u001b[?1006l\u001b[?2004l\u001b[?1004l\u001b[?25h\u001b[0m";
        const string Wipe = "\u001b[0m\u001b[H\u001b[2J\u001b[3J";
        static readonly TimeSpan SizeCheck = TimeSpan.FromMilliseconds(250);
        /// How often the frames loop looks at the size of this tab and at its timers.
        static readonly TimeSpan Housekeeping = TimeSpan.FromMilliseconds(250);
        /// How often the service is asked for the stream again while the Ward of the session is
        /// not there, for how long while the session starts, and how long a Ward that went away
        /// is waited for.
        public static readonly TimeSpan ResumeEvery = TimeSpan.FromSeconds(20);
        public static readonly TimeSpan AskFor = TimeSpan.FromMinutes(10);
        public static readonly TimeSpan GiveUpAfter = TimeSpan.FromSeconds(60);

        [DllImport("kernel32.dll", SetLastError = true)]
        static extern IntPtr GetStdHandle(int which);

        [DllImport("kernel32.dll", SetLastError = true)]
        static extern bool GetConsoleMode(IntPtr handle, out uint mode);

        [DllImport("kernel32.dll", SetLastError = true)]
        static extern bool SetConsoleMode(IntPtr handle, uint mode);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        static extern bool ReadConsoleW(IntPtr handle, [Out] char[] buffer, uint toRead, out uint read, IntPtr control);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        static extern bool WriteConsoleW(IntPtr handle, string text, uint length, out uint written, IntPtr reserved);

        static readonly object writing = new object();
        // ClientWebSocket takes one send at a time: keys and sizes go from two threads.
        static readonly object sending = new object();
        static readonly object sizing = new object();
        static IntPtr output;
        static bool consoleOut;
        static Stream rawOut;
        static volatile bool detached;
        static int ownCols;
        static int ownRows;
        static string machine;
        static Link link;
        // Status lines of the viewer are on the screen, and nothing of the session yet.
        static bool statusOnScreen;
        // Something of the session was drawn since its screen started.
        static bool sessionOnScreen;
        // When to ask the service for the stream again, until when to ask, and when to stop
        // waiting for a Ward that went away.
        static DateTime resumeAt = DateTime.MaxValue;
        static DateTime askUntil = DateTime.MaxValue;
        static DateTime giveUpAt = DateTime.MaxValue;

        public static int Run(string url, string title, string worker)
        {
            IntPtr input = GetStdHandle(StdInput);
            output = GetStdHandle(StdOutput);
            uint inMode;
            uint outMode;
            bool consoleIn = GetConsoleMode(input, out inMode);
            consoleOut = GetConsoleMode(output, out outMode);
            rawOut = Console.OpenStandardOutput();
            if (consoleOut && !SetConsoleMode(output, outMode | ProcessedOutput | VirtualTerminalProcessing | NoAutoReturn))
            {
                SetConsoleMode(output, outMode | ProcessedOutput | VirtualTerminalProcessing);
            }

            machine = worker;
            link = Link.Ended;
            Enter(Link.Connecting);
            ClientWebSocket socket = new ClientWebSocket();
            socket.Options.KeepAliveInterval = TimeSpan.FromSeconds(20);
            try
            {
                socket.ConnectAsync(new Uri(url), CancellationToken.None).GetAwaiter().GetResult();
            }
            catch (Exception e)
            {
                Finish("не удалось подключиться: " + Reason(e)
                    + ". Билет на подключение одноразовый: нажмите «Подключиться» в карточке ещё раз.");
                return 1;
            }
            Say("Ctrl+] — отключиться; сессия на своей машине продолжит работу.");

            int cols;
            int rows;
            if (ReadSize(out cols, out rows))
            {
                SetOwnSize(cols, rows);
                SendSize(socket);
            }
            Thread sizes = new Thread(delegate () { WatchSize(socket); });
            sizes.IsBackground = true;
            sizes.Start();

            if (consoleIn)
            {
                uint raw = (inMode & ~(ProcessedInput | LineInput | EchoInput | WindowInput | MouseInput | QuickEdit))
                    | ExtendedFlags | VirtualTerminalInput;
                if (!SetConsoleMode(input, raw))
                {
                    SetConsoleMode(input, inMode & ~(ProcessedInput | LineInput | EchoInput));
                }
            }
            Thread keys = new Thread(delegate () { Pump(socket, input, consoleIn); });
            keys.IsBackground = true;
            keys.Start();

            int code;
            try
            {
                code = Receive(socket);
            }
            finally
            {
                if (consoleIn)
                {
                    SetConsoleMode(input, inMode);
                }
                Write(Restore);
                if (consoleOut)
                {
                    SetConsoleMode(output, outMode);
                }
            }
            return code;
        }

        /// The output of the session's tab and the service frames, until the stream is over.
        static int Receive(ClientWebSocket socket)
        {
            byte[] buffer = new byte[64 * 1024];
            char[] chars = new char[64 * 1024 + 8];
            Decoder decoder = new UTF8Encoding(false).GetDecoder();
            Filter filter = new Filter();
            MemoryStream text = new MemoryStream();
            int cols;
            int rows;
            GetOwnSize(out cols, out rows);
            Screen screen = new Screen(cols, rows);
            Task<WebSocketReceiveResult> pending = null;
            while (true)
            {
                WebSocketReceiveResult result = null;
                try
                {
                    if (pending == null)
                    {
                        pending = socket.ReceiveAsync(new ArraySegment<byte>(buffer), CancellationToken.None);
                    }
                    TimeSpan wait = screen.Left(DateTime.UtcNow);
                    if (wait == Timeout.InfiniteTimeSpan || wait > Housekeeping)
                    {
                        wait = Housekeeping;
                    }
                    if (pending.Wait(wait))
                    {
                        result = pending.GetAwaiter().GetResult();
                        pending = null;
                    }
                }
                catch (Exception e)
                {
                    if (detached)
                    {
                        Finish("отключено. Сессия на своей машине продолжает работу.");
                        return 0;
                    }
                    Finish("связь прервалась: " + Reason(e));
                    return 1;
                }
                DateTime now = DateTime.UtcNow;
                GetOwnSize(out cols, out rows);
                screen.Own(cols, rows, now);
                if (result == null)
                {
                    Apply(screen.Tick(now), screen, decoder, filter, chars);
                    if (now >= giveUpAt)
                    {
                        Finish("трансляция закончилась: Ward на " + MachineName() + " не вернулся за минуту. Подключиться снова — кнопкой в карточке.");
                        Close(socket);
                        return 0;
                    }
                    if (now >= resumeAt)
                    {
                        if (now < askUntil)
                        {
                            SendResume(socket);
                        }
                        resumeAt = now < askUntil ? now + ResumeEvery : DateTime.MaxValue;
                    }
                    continue;
                }
                if (result.MessageType == WebSocketMessageType.Close)
                {
                    Finish(detached ? "отключено. Сессия на своей машине продолжает работу." : "трансляция закончилась.");
                    Close(socket);
                    return 0;
                }
                if (result.MessageType == WebSocketMessageType.Binary)
                {
                    byte[] chunk = new byte[result.Count];
                    Buffer.BlockCopy(buffer, 0, chunk, 0, result.Count);
                    Apply(screen.Output(chunk, now), screen, decoder, filter, chars);
                    Heard(screen);
                    Apply(screen.Tick(now), screen, decoder, filter, chars);
                    continue;
                }
                text.Write(buffer, 0, result.Count);
                if (!result.EndOfMessage)
                {
                    continue;
                }
                string frame = Encoding.UTF8.GetString(text.ToArray());
                text.SetLength(0);
                string type = Frames.Field(frame, "type");
                if (type == "hello")
                {
                    screen.Hello(Frames.Has(frame, "size"), Frames.Has(frame, "redraw"));
                    Heard(screen);
                }
                else if (type == "waiting")
                {
                    // The service asked the Ward of the session for the stream; a Ward that
                    // does not know the session yet lets the command go, so it is asked again
                    // while the session starts.
                    if (link != Link.Reconnecting)
                    {
                        Enter(Link.Waiting);
                    }
                    if (askUntil == DateTime.MaxValue)
                    {
                        askUntil = now + AskFor;
                        resumeAt = now + ResumeEvery;
                    }
                }
                else if (type == "reset")
                {
                    // The Ward of the session connected again: its screen starts over, and it
                    // is told the size of this tab again.
                    if (sessionOnScreen)
                    {
                        Write("\u001b[0m\u001b[H\u001b[2J");
                    }
                    decoder.Reset();
                    filter.Reset();
                    sessionOnScreen = false;
                    screen.Reset();
                    StopAsking();
                    Enter(Link.Connecting);
                    SendSize(socket);
                }
                else if (type == "size")
                {
                    Apply(screen.Size(Frames.Number(frame, "cols"), Frames.Number(frame, "rows"), now), screen, decoder, filter, chars);
                    Heard(screen);
                }
                else if (type == "ended")
                {
                    // The Ward of the session went away; it comes back with a new connection
                    // (reset) when the service asks it again.
                    Enter(Link.Reconnecting);
                    SendResume(socket);
                    resumeAt = now + ResumeEvery;
                    giveUpAt = now + GiveUpAfter;
                    askUntil = giveUpAt;
                }
                else if (type == "error")
                {
                    string message = Frames.Field(frame, "message");
                    Finish(string.IsNullOrEmpty(message) ? "трансляция недоступна." : message);
                    Close(socket);
                    return 1;
                }
                Apply(screen.Tick(now), screen, decoder, filter, chars);
            }
        }

        /// The Ward of the session is there: it said hello, or its screen came.
        static void Heard(Screen screen)
        {
            if (!screen.Introduced && !screen.Heard)
            {
                return;
            }
            StopAsking();
            if (screen.Legacy)
            {
                Enter(Link.Legacy);
            }
            else if (link != Link.Live)
            {
                Enter(Link.Live);
            }
        }

        static void StopAsking()
        {
            resumeAt = DateTime.MaxValue;
            askUntil = DateTime.MaxValue;
            giveUpAt = DateTime.MaxValue;
        }

        /// Does what the screen asks.
        static void Apply(Step step, Screen screen, Decoder decoder, Filter filter, char[] chars)
        {
            if (step.Clear)
            {
                Write(Wipe);
                statusOnScreen = false;
                decoder.Reset();
                filter.Reset();
            }
            if (step.Draw.Count > 0 && statusOnScreen && !sessionOnScreen)
            {
                // The lines of the viewer were for the time before the screen; the header of
                // the tab goes on saying where the stream is.
                Write(Wipe);
                statusOnScreen = false;
            }
            if (step.Warn)
            {
                WarnIfSmaller(screen.RemoteCols, screen.RemoteRows);
            }
            foreach (byte[] chunk in step.Draw)
            {
                int offset = 0;
                while (offset < chunk.Length)
                {
                    int count = Math.Min(chunk.Length - offset, 64 * 1024);
                    int decoded = decoder.GetChars(chunk, offset, count, chars, 0, false);
                    if (decoded > 0)
                    {
                        Write(filter.Feed(new string(chars, 0, decoded)));
                        sessionOnScreen = true;
                    }
                    offset += count;
                }
            }
        }

        /// A new state of the stream: in the title for the header of the tab, and as a line here
        /// while nothing of the session is on the screen to be spoiled by it.
        static void Enter(Link next)
        {
            if (link == next)
            {
                return;
            }
            link = next;
            SetTitle(Status.Title(next, machine));
            if (!sessionOnScreen)
            {
                Say(Status.Text(next, machine));
                statusOnScreen = true;
            }
        }

        static string MachineName()
        {
            return string.IsNullOrEmpty(machine) ? "машине сессии" : machine;
        }

        /// The keys typed here go to the session's tab as they are: Enter, Ctrl+C, Esc, arrows.
        static void Pump(ClientWebSocket socket, IntPtr input, bool consoleIn)
        {
            Encoder encoder = new UTF8Encoding(false).GetEncoder();
            char[] chars = new char[1024];
            byte[] bytes = new byte[4 * 1024 + 16];
            Stream rawIn = consoleIn ? null : Console.OpenStandardInput();
            while (socket.State == WebSocketState.Open)
            {
                int count;
                bool stop = false;
                if (consoleIn)
                {
                    uint read;
                    if (!ReadConsoleW(input, chars, (uint)chars.Length, out read, IntPtr.Zero))
                    {
                        return;
                    }
                    int length = (int)read;
                    int cut = Array.IndexOf(chars, Detach, 0, length);
                    if (cut >= 0)
                    {
                        length = cut;
                        stop = true;
                    }
                    count = encoder.GetBytes(chars, 0, length, bytes, 0, stop);
                }
                else
                {
                    count = rawIn.Read(bytes, 0, 4 * 1024);
                    if (count <= 0)
                    {
                        return;
                    }
                    int cut = Array.IndexOf(bytes, (byte)0x1d, 0, count);
                    if (cut >= 0)
                    {
                        count = cut;
                        stop = true;
                    }
                }
                if (count > 0 && !Send(socket, bytes, count, WebSocketMessageType.Binary))
                {
                    return;
                }
                if (stop)
                {
                    detached = true;
                    lock (sending)
                    {
                        try
                        {
                            socket.CloseOutputAsync(WebSocketCloseStatus.NormalClosure, "detached", CancellationToken.None).Wait(2000);
                        }
                        catch { }
                    }
                    try { socket.Abort(); } catch { }
                    return;
                }
            }
        }

        /// The size of this tab as it changes: the session's tab follows it.
        static void WatchSize(ClientWebSocket socket)
        {
            while (!detached && socket.State == WebSocketState.Open)
            {
                Thread.Sleep(SizeCheck);
                int cols;
                int rows;
                int oldCols;
                int oldRows;
                GetOwnSize(out oldCols, out oldRows);
                if (!ReadSize(out cols, out rows) || (cols == oldCols && rows == oldRows))
                {
                    continue;
                }
                SetOwnSize(cols, rows);
                SendSize(socket);
            }
        }

        static void GetOwnSize(out int cols, out int rows)
        {
            lock (sizing)
            {
                cols = ownCols;
                rows = ownRows;
            }
        }

        static void SetOwnSize(int cols, int rows)
        {
            lock (sizing)
            {
                ownCols = cols;
                ownRows = rows;
            }
        }

        static bool ReadSize(out int cols, out int rows)
        {
            cols = 0;
            rows = 0;
            if (!consoleOut)
            {
                return false;
            }
            try
            {
                cols = Console.WindowWidth;
                rows = Console.WindowHeight;
            }
            catch
            {
                return false;
            }
            return cols > 0 && rows > 0;
        }

        static void SendSize(ClientWebSocket socket)
        {
            int cols;
            int rows;
            GetOwnSize(out cols, out rows);
            if (cols <= 0 || rows <= 0)
            {
                return;
            }
            byte[] frame = Encoding.UTF8.GetBytes("{\"type\":\"size\",\"cols\":" + cols + ",\"rows\":" + rows + "}");
            Send(socket, frame, frame.Length, WebSocketMessageType.Text);
        }

        /// Asks the service for the stream again: the Ward of the session is not connected.
        static void SendResume(ClientWebSocket socket)
        {
            byte[] frame = Encoding.UTF8.GetBytes("{\"type\":\"resume\"}");
            Send(socket, frame, frame.Length, WebSocketMessageType.Text);
        }

        static bool Send(ClientWebSocket socket, byte[] bytes, int count, WebSocketMessageType type)
        {
            lock (sending)
            {
                try
                {
                    socket.SendAsync(new ArraySegment<byte>(bytes, 0, count), type, true, CancellationToken.None).GetAwaiter().GetResult();
                    return true;
                }
                catch
                {
                    return false;
                }
            }
        }

        static void WarnIfSmaller(int cols, int rows)
        {
            if (cols <= 0 || rows <= 0 || !consoleOut)
            {
                return;
            }
            int width;
            int height;
            try
            {
                width = Console.WindowWidth;
                height = Console.WindowHeight;
            }
            catch
            {
                return;
            }
            if (width < cols || height < rows)
            {
                Say("экран сессии " + cols + "×" + rows + ", а вкладка здесь " + width + "×" + height
                    + ": растяните окно или уменьшите шрифт, иначе картинка разъедется.");
            }
        }

        static void Close(ClientWebSocket socket)
        {
            try
            {
                if (socket.State == WebSocketState.Open || socket.State == WebSocketState.CloseReceived)
                {
                    socket.CloseAsync(WebSocketCloseStatus.NormalClosure, "", CancellationToken.None).Wait(2000);
                }
            }
            catch { }
        }

        static string Reason(Exception e)
        {
            while ((e is AggregateException || e.Message.Length == 0) && e.InnerException != null)
            {
                e = e.InnerException;
            }
            string reason = e.Message.TrimEnd('.', ' ');
            if (e.InnerException != null && e.InnerException.Message != reason)
            {
                reason += " (" + e.InnerException.Message.TrimEnd('.', ' ') + ")";
            }
            return reason;
        }

        static void Finish(string text)
        {
            link = Link.Ended;
            SetTitle(Status.Title(Link.Ended, machine));
            Write(Restore);
            Say(text);
        }

        static void Say(string text)
        {
            Write("\r\n\u001b[36mWard: " + text + "\u001b[0m\r\n");
        }

        static void SetTitle(string text)
        {
            if (!consoleOut)
            {
                return;
            }
            try
            {
                Console.Title = text;
            }
            catch { }
        }

        static void Write(string text)
        {
            if (text.Length == 0)
            {
                return;
            }
            lock (writing)
            {
                int offset = 0;
                while (consoleOut && offset < text.Length)
                {
                    string part = offset == 0 ? text : text.Substring(offset);
                    uint written;
                    if (!WriteConsoleW(output, part, (uint)part.Length, out written, IntPtr.Zero) || written == 0)
                    {
                        consoleOut = false;
                        break;
                    }
                    offset += (int)written;
                }
                if (offset >= text.Length)
                {
                    return;
                }
                byte[] bytes = Encoding.UTF8.GetBytes(offset == 0 ? text : text.Substring(offset));
                rawOut.Write(bytes, 0, bytes.Length);
                rawOut.Flush();
            }
        }
    }
}
'@

if (-not ('WardAttach.Viewer' -as [type])) {
    Add-Type -TypeDefinition $source -Language CSharp -IgnoreWarnings
}
$code = [WardAttach.Viewer]::Run($url, $title, $worker)
exit $code
