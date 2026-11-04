# Ward: the tab of an agent session that runs on another machine (WARD.md, "Подключиться").
#
# Ward writes this file and runs it in a tab of its own with WARD_ATTACH_URL (the viewer socket
# of the agents service with a one-time ticket) and WARD_ATTACH_TITLE. The viewer connects to
# the socket, prints the raw output of the session's tab as it comes and sends the keys typed
# here back as binary frames, as the viewer in the browser does. Ctrl+] disconnects; the
# session on its own machine goes on.

$ErrorActionPreference = 'Stop'
$url = $env:WARD_ATTACH_URL
$title = [string]$env:WARD_ATTACH_TITLE
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
using System.IO;
using System.Net.WebSockets;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.RegularExpressions;
using System.Threading;

namespace WardAttach
{
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
        static IntPtr output;
        static bool consoleOut;
        static Stream rawOut;
        static volatile bool detached;

        public static int Run(string url, string title)
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
            Say("подключение к сессии" + name + "…");
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
            MemoryStream text = new MemoryStream();
            bool drawn = false;
            bool sized = false;
            while (true)
            {
                WebSocketReceiveResult result;
                try
                {
                    result = socket.ReceiveAsync(new ArraySegment<byte>(buffer), CancellationToken.None).GetAwaiter().GetResult();
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
                    int count = decoder.GetChars(buffer, 0, result.Count, chars, 0, false);
                    if (count > 0)
                    {
                        Write(new string(chars, 0, count));
                        drawn = true;
                    }
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
                    // The Ward of the session connected again: its screen starts over.
                    if (drawn)
                    {
                        Write("\u001b[0m\u001b[H\u001b[2J");
                    }
                    decoder.Reset();
                    drawn = false;
                }
                else if (type == "size")
                {
                    if (!sized && !drawn)
                    {
                        sized = true;
                        WarnIfSmaller(Number(frame, "cols"), Number(frame, "rows"));
                    }
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
                if (count > 0)
                {
                    try
                    {
                        socket.SendAsync(new ArraySegment<byte>(bytes, 0, count), WebSocketMessageType.Binary, true, CancellationToken.None).GetAwaiter().GetResult();
                    }
                    catch
                    {
                        return;
                    }
                }
                if (stop)
                {
                    detached = true;
                    try
                    {
                        socket.CloseOutputAsync(WebSocketCloseStatus.NormalClosure, "detached", CancellationToken.None).Wait(2000);
                    }
                    catch { }
                    try { socket.Abort(); } catch { }
                    return;
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
$code = [WardAttach.Viewer]::Run($url, $title)
exit $code
