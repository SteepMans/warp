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
# not drawn (see Screen): what was drawn for another size is dropped, the redraw is shown. A
# machine that never takes the size (an older Ward or service) gets the old behavior after a few
# seconds: what came is drawn as it is.

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
    /// When the sizes do not meet within HoldFor (an older Ward or service), what was held is
    /// drawn as it is.
    public sealed class Screen
    {
        public static readonly TimeSpan HoldFor = TimeSpan.FromSeconds(3);
        const int HeldCap = 4 * 1024 * 1024;

        readonly List<byte[]> held = new List<byte[]>();
        int heldBytes;
        bool holding;
        bool sized;
        bool foreign;
        DateTime deadline = DateTime.MaxValue;
        int remoteCols;
        int remoteRows;

        /// Without the size of this tab there is nothing to wait for: output is drawn at once.
        public Screen(bool knowsOwnSize)
        {
            holding = knowsOwnSize;
        }

        public bool Holding { get { return holding; } }
        public int RemoteCols { get { return remoteCols; } }
        public int RemoteRows { get { return remoteRows; } }

        /// The session's machine starts its screen over (it connected again).
        public void Reset(bool knowsOwnSize)
        {
            held.Clear();
            heldBytes = 0;
            holding = knowsOwnSize;
            sized = false;
            foreign = false;
            deadline = DateTime.MaxValue;
        }

        /// A piece of output of the session's tab.
        public Step Output(byte[] chunk, DateTime now)
        {
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

        /// The size of the session's tab, against the size of this one.
        public Step Size(int cols, int rows, int ownCols, int ownRows, DateTime now)
        {
            Step step = new Step();
            remoteCols = cols;
            remoteRows = rows;
            if (!holding)
            {
                return step;
            }
            sized = true;
            if (cols != ownCols || rows != ownRows)
            {
                foreign = true;
                StartWaiting(now);
                return step;
            }
            holding = false;
            deadline = DateTime.MaxValue;
            if (foreign)
            {
                step.Clear = true;
            }
            else
            {
                step.Draw.AddRange(held);
            }
            held.Clear();
            heldBytes = 0;
            return step;
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
            holding = false;
            deadline = DateTime.MaxValue;
            step.Warn = foreign;
            step.Draw.AddRange(held);
            held.Clear();
            heldBytes = 0;
            return step;
        }

        void StartWaiting(DateTime now)
        {
            if (deadline == DateTime.MaxValue)
            {
                deadline = now + HoldFor;
            }
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
        static IntPtr output;
        static bool consoleOut;
        static Stream rawOut;
        static volatile bool detached;
        static volatile int ownCols;
        static volatile int ownRows;

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

            string name = string.IsNullOrEmpty(title) ? "" : " «" + title + "»";
            string machine = string.IsNullOrEmpty(worker) ? "" : " на " + worker;
            Say("подключение к сессии" + name + machine + "…");
            ClientWebSocket socket = new ClientWebSocket();
            socket.Options.KeepAliveInterval = TimeSpan.FromSeconds(20);
            try
            {
                socket.ConnectAsync(new Uri(url), CancellationToken.None).GetAwaiter().GetResult();
            }
            catch (Exception e)
            {
                Say("не удалось подключиться: " + Reason(e)
                    + ". Билет на подключение одноразовый: нажмите «Подключиться» в карточке ещё раз.");
                return 1;
            }
            Say("Ctrl+] — отключиться; сессия на своей машине продолжит работу.");

            int cols;
            int rows;
            if (ReadSize(out cols, out rows))
            {
                ownCols = cols;
                ownRows = rows;
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
            Screen screen = new Screen(ownCols > 0);
            Task<WebSocketReceiveResult> pending = null;
            bool drawn = false;
            while (true)
            {
                WebSocketReceiveResult result;
                try
                {
                    if (pending == null)
                    {
                        pending = socket.ReceiveAsync(new ArraySegment<byte>(buffer), CancellationToken.None);
                    }
                    if (!pending.Wait(screen.Left(DateTime.UtcNow)))
                    {
                        drawn |= Apply(screen.Tick(DateTime.UtcNow), screen, decoder, filter, chars);
                        continue;
                    }
                    result = pending.GetAwaiter().GetResult();
                    pending = null;
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
                    drawn |= Apply(screen.Output(chunk, DateTime.UtcNow), screen, decoder, filter, chars);
                    continue;
                }
                text.Write(buffer, 0, result.Count);
                if (!result.EndOfMessage)
                {
                    continue;
                }
                string frame = Encoding.UTF8.GetString(text.ToArray());
                text.SetLength(0);
                string type = Field(frame, "type");
                if (type == "waiting")
                {
                    if (!drawn)
                    {
                        Say("ждём Ward на машине сессии…");
                    }
                }
                else if (type == "reset")
                {
                    // The Ward of the session connected again: its screen starts over, and it
                    // is told the size of this tab again.
                    if (drawn)
                    {
                        Write("\u001b[0m\u001b[H\u001b[2J");
                    }
                    decoder.Reset();
                    filter.Reset();
                    drawn = false;
                    screen.Reset(ownCols > 0);
                    SendSize(socket);
                }
                else if (type == "size")
                {
                    Step step = screen.Size(Number(frame, "cols"), Number(frame, "rows"), ownCols, ownRows, DateTime.UtcNow);
                    drawn |= Apply(step, screen, decoder, filter, chars);
                }
                else if (type == "ended")
                {
                    Finish("Ward на машине сессии закончил трансляцию. Подключиться снова — кнопкой в карточке.");
                    Close(socket);
                    return 0;
                }
                else if (type == "error")
                {
                    string message = Field(frame, "message");
                    Finish(string.IsNullOrEmpty(message) ? "трансляция недоступна." : message);
                    Close(socket);
                    return 1;
                }
            }
        }

        /// Does what the screen asks; true when anything of the session was drawn.
        static bool Apply(Step step, Screen screen, Decoder decoder, Filter filter, char[] chars)
        {
            if (step.Clear)
            {
                Write(Wipe);
                decoder.Reset();
                filter.Reset();
            }
            if (step.Warn)
            {
                WarnIfSmaller(screen.RemoteCols, screen.RemoteRows);
            }
            bool drawn = false;
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
                        drawn = true;
                    }
                    offset += count;
                }
            }
            return drawn;
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
                if (!ReadSize(out cols, out rows) || (cols == ownCols && rows == ownRows))
                {
                    continue;
                }
                ownCols = cols;
                ownRows = rows;
                SendSize(socket);
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
            int cols = ownCols;
            int rows = ownRows;
            if (cols <= 0 || rows <= 0)
            {
                return;
            }
            byte[] frame = Encoding.UTF8.GetBytes("{\"type\":\"size\",\"cols\":" + cols + ",\"rows\":" + rows + "}");
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

        static string Field(string json, string name)
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

        static int Number(string json, string name)
        {
            Match match = Regex.Match(json, "\"" + name + "\"\\s*:\\s*(\\d+)");
            int value;
            return match.Success && int.TryParse(match.Groups[1].Value, out value) ? value : 0;
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
            Write(Restore);
            Say(text);
        }

        static void Say(string text)
        {
            Write("\r\n\u001b[36mWard: " + text + "\u001b[0m\r\n");
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
