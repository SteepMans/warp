// The checks of the viewer's C# (attach.ps1), compiled after it into one source: it has the
// usings. attach_tests.rs runs them under Windows PowerShell, as the tab runs the viewer, and
// reads the file Suite.Run writes: a line per check and the title of every state of the stream.

namespace WardAttachTests
{
    using WardAttach;

    public static class Suite
    {
        const string E = "\u001b";
        const string Bel = "\u0007";
        const string St = "\u001b\\";

        static readonly List<string> lines = new List<string>();
        static int failures;

        public static int Run(string outPath)
        {
            Check("the filter keeps what draws the screen", FilterKeepsTheScreen);
            Check("the filter drops queries and reports", FilterDropsQueries);
            Check("the filter drops the marks of the other terminal", FilterDropsMarks);
            Check("the filter drops the keyboard and window modes of the other terminal", FilterDropsModes);
            Check("the filter keeps links", FilterKeepsLinks);
            Check("the filter puts together sequences cut between pieces", FilterJoinsPieces);
            Check("an escape ends an unfinished string", FilterEscapeEndsAString);
            Check("hello is read with its features", FramesReadHello);
            Check("a newer Ward that takes another size: the redraw is drawn", ScreenRedrawAtAnotherSize);
            Check("a newer Ward at the same size: the redraw is drawn, not the recent output", ScreenRedrawAtTheSameSize);
            Check("a newer Ward whose redraw does not come: the recent output after a while", ScreenRedrawThatDoesNotCome);
            Check("output before any size is not drawn at a foreign size", ScreenOutputBeforeAnySize);
            Check("an older Ward at another size: the output as it is after HoldFor", ScreenOlderWardAtAnotherSize);
            Check("an older Ward at the same size: the output at once", ScreenOlderWardAtTheSameSize);
            Check("this tab resized: held until the session's tab follows", ScreenOwnResize);
            Check("the session's tab took a size of another viewer", ScreenAnotherViewer);
            Check("a new connection of the Ward starts the screen over", ScreenReset);
            Check("without the size of this tab output is drawn at once", ScreenWithoutOwnSize);
            Check("the states of the stream are said with the machine", StatusTexts);

            foreach (Link link in Enum.GetValues(typeof(Link)))
            {
                lines.Add("title\t" + link + "\t" + Status.Title(link, "steep-pc"));
            }
            File.WriteAllLines(outPath, lines.ToArray(), new UTF8Encoding(false));
            return failures == 0 ? 0 : 1;
        }

        static void FilterKeepsTheScreen()
        {
            string screen = "a" + E + "[31mb" + E + "[0m" + E + "[2;3H" + E + "[?25l" + E + "[?1049h"
                + E + "7" + E + "[r" + E + "8" + E + "[1;1H\r\n" + E + "[2K" + E + "[1A" + E + "[J"
                + E + "[?2026h" + E + "[?2026l" + E + "[2 q" + E + "[u" + E + "[s" + E + "(B" + "я ✳";
            Equal(screen, Feed(screen));
        }

        static void FilterDropsQueries()
        {
            string[] queries = new string[]
            {
                E + "[c", E + "[0c", E + "[>c", E + "[>0c", E + "[=c", E + "[5n", E + "[6n", E + "[?6n",
                E + "[>0q", E + "[?u", E + "[?2026$p", E + "[?1016$p", E + "[16t", E + "[14t",
                E + "[18t", E + "[22;0t", E + "[23;0t", E + "[8;50;200t", E + "[x", E + "Z",
                E + "]10;?" + Bel, E + "]11;?" + St, E + "]7501;?" + St, E + "]4;1;?" + Bel,
                E + "P$qm" + St, E + "P+q544e" + St, E + "_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA" + St,
                E + "[?2;1;0S", E + "[1;1;1;1;1*y",
            };
            foreach (string query in queries)
            {
                Equal("ab", Feed("a" + query + "b"));
            }
        }

        static void FilterDropsMarks()
        {
            string[] marks = new string[]
            {
                E + "]0;✳ Claude Code" + Bel,
                E + "]2;title" + St,
                E + "]9278;d;7b2274797065223a22507265636d64227d" + Bel,
                E + "]9279;a" + Bel,
                E + "]777;notify;warp://cli-agent;{\"event\":\"session_start\"}" + Bel,
                E + "]133;A" + Bel,
                E + "]133;B" + St,
                E + "]7;file://steep-pc/C:/work" + St,
                E + "]52;c;aGVsbG8=" + Bel,
                E + "]1337;SetUserVar=a=b" + Bel,
            };
            foreach (string mark in marks)
            {
                Equal("ab", Feed("a" + mark + "b"));
            }
        }

        static void FilterDropsModes()
        {
            string[] modes = new string[]
            {
                E + "[>5u", E + "[<u", E + "[<3u", E + "[=1;1u", E + "[>4;2m", E + "[>4m", E + "[?4m",
                E + "[?9001h", E + "[?9001l", E + "[?2031h", E + "[?2048h", E + "[?1016h",
            };
            foreach (string mode in modes)
            {
                Equal("ab", Feed("a" + mode + "b"));
            }
            Equal(E + "[?2004h" + E + "[?1004h" + E + "[?1000h" + E + "[?1006h",
                Feed(E + "[?2004h" + E + "[?1004h" + E + "[?1000h" + E + "[?1006h"));
            Equal(E + "[?1004;2004h", Feed(E + "[?1004;9001;2004h"));
            Equal(E + "[4h" + E + "[20l", Feed(E + "[4h" + E + "[20l"));
        }

        static void FilterKeepsLinks()
        {
            string link = E + "]8;;https://chimney.steep-man.ru/" + St + "ссылка" + E + "]8;;" + St;
            Equal(link, Feed(link));
            string bell = E + "]8;id=1;https://example.com" + Bel + "x" + E + "]8;;" + Bel;
            Equal(bell, Feed(bell));
        }

        static void FilterJoinsPieces()
        {
            Equal("ab" + E + "[31mc", Feed("a" + E + "[", "?9001", "hb" + E + "]0;ti", "tle" + Bel + E, "[31mc"));
            Equal("x" + E + "[1;1H", Feed("x" + E, "[1;", "1H"));
            Equal("12", Feed("1" + E + "_Gi=1;", "AAAA" + E, "\\2"));
            Equal(E + "]8;;u" + St + "t", Feed(E + "]8", ";;u" + E, "\\t"));
        }

        static void FilterEscapeEndsAString()
        {
            Equal(E + "[31mz", Feed(E + "]0;x" + E + "[31mz"));
            Equal(E + "[32mz", Feed(E + "Pq#0;2;0;0;0" + E + "[32mz"));
            Equal("az", Feed("a" + E + "[1;2" + "\u0018" + "z"));
        }

        static void FramesReadHello()
        {
            string hello = "{\"type\":\"hello\",\"version\":\"v0.2026.11.05\",\"features\":[\"size\",\"redraw\"]}";
            Equal("hello", Frames.Field(hello, "type"));
            True(Frames.Has(hello, "size"), "size");
            True(Frames.Has(hello, "redraw"), "redraw");
            True(!Frames.Has(hello, "keys"), "keys");
            True(!Frames.Has("{\"type\":\"hello\",\"version\":\"x\"}", "size"), "no features");
            True(!Frames.Has("{\"type\":\"size\",\"cols\":120,\"rows\":40}", "size"), "a size frame");
            True(Frames.Has("{\"type\":\"hello\", \"features\" : [ \"redraw\" , \"size\" ]}", "size"), "spaces");
            Equal("120", Frames.Number("{\"type\":\"size\",\"cols\":120,\"rows\":40}", "cols").ToString());
        }

        static void ScreenRedrawAtAnotherSize()
        {
            DateTime t = Start();
            Screen s = new Screen(100, 30);
            s.Hello(true, true);
            Nothing(s.Size(120, 40, t));
            Nothing(s.Output(B("recent output at 120x40"), t));
            True(s.Holding && !s.Legacy, "holding, a newer Ward");
            Step step = s.Size(100, 30, t.AddMilliseconds(200));
            True(step.Clear, "the screen is wiped");
            Equal("", Drawn(step));
            Equal("redraw", Drawn(s.Output(B("redraw"), t.AddMilliseconds(300))));
        }

        static void ScreenRedrawAtTheSameSize()
        {
            DateTime t = Start();
            Screen s = new Screen(100, 30);
            s.Hello(true, true);
            Nothing(s.Size(100, 30, t));
            True(s.Holding, "the redraw is waited for");
            Nothing(s.Output(B("recent output"), t));
            Nothing(s.Size(100, 29, t.AddMilliseconds(100)));
            Nothing(s.Output(B("at 100x29"), t.AddMilliseconds(150)));
            Step step = s.Size(100, 30, t.AddMilliseconds(400));
            True(step.Clear, "the screen is wiped");
            Equal("", Drawn(step));
            Equal("redraw", Drawn(s.Output(B("redraw"), t.AddMilliseconds(450))));
            True(!s.Holding, "live");
        }

        static void ScreenRedrawThatDoesNotCome()
        {
            DateTime t = Start();
            Screen s = new Screen(100, 30);
            s.Hello(true, true);
            Nothing(s.Size(100, 30, t));
            Nothing(s.Output(B("recent output"), t));
            Nothing(s.Tick(t + Screen.RedrawFor - TimeSpan.FromMilliseconds(1)));
            Step step = s.Tick(t + Screen.RedrawFor);
            True(!step.Clear && !step.Warn, "drawn as it is, at the same size");
            Equal("recent output", Drawn(step));
            Equal("more", Drawn(s.Output(B("more"), t + Screen.RedrawFor)));
        }

        static void ScreenOutputBeforeAnySize()
        {
            DateTime t = Start();
            Screen s = new Screen(100, 30);
            s.Hello(true, true);
            Nothing(s.Output(B("of any size"), t));
            Step step = s.Size(100, 30, t);
            True(step.Clear, "wiped");
            Equal("", Drawn(step));
        }

        static void ScreenOlderWardAtAnotherSize()
        {
            DateTime t = Start();
            Screen s = new Screen(100, 30);
            Nothing(s.Size(205, 61, t));
            True(s.Legacy, "an older Ward");
            Nothing(s.Output(B("recent output at 205x61"), t));
            Nothing(s.Tick(t + Screen.HoldFor - TimeSpan.FromMilliseconds(1)));
            Step step = s.Tick(t + Screen.HoldFor);
            True(step.Warn && !step.Clear, "drawn as it is, with a warning");
            Equal("recent output at 205x61", Drawn(step));
            Equal("more", Drawn(s.Output(B("more"), t + Screen.HoldFor)));
            // An older Ward does not follow this tab: nothing to wait for.
            s.Own(90, 25, t + Screen.HoldFor);
            Equal("after", Drawn(s.Output(B("after"), t + Screen.HoldFor)));
        }

        static void ScreenOlderWardAtTheSameSize()
        {
            DateTime t = Start();
            Screen s = new Screen(100, 30);
            Nothing(s.Size(100, 30, t));
            True(s.Legacy && !s.Holding, "an older Ward, nothing to wait for");
            Equal("recent output", Drawn(s.Output(B("recent output"), t)));
        }

        static void ScreenOwnResize()
        {
            DateTime t = Start();
            Screen s = Live(t);
            s.Own(90, 25, t);
            True(s.Holding, "held after the resize");
            Nothing(s.Output(B("for 100x30"), t));
            Nothing(s.Size(100, 30, t));
            Step step = s.Size(90, 25, t.AddMilliseconds(300));
            True(step.Clear, "wiped");
            Equal("redraw", Drawn(s.Output(B("redraw"), t.AddMilliseconds(350))));
        }

        static void ScreenAnotherViewer()
        {
            DateTime t = Start();
            Screen s = Live(t);
            Nothing(s.Size(80, 20, t));
            True(s.Holding, "held for another size");
            Nothing(s.Output(B("for 80x20"), t));
            Step step = s.Tick(t + Screen.RedrawFor);
            True(step.Warn, "a warning");
            Equal("for 80x20", Drawn(step));
        }

        static void ScreenReset()
        {
            DateTime t = Start();
            Screen s = Live(t);
            s.Reset();
            True(s.Holding && !s.Introduced && !s.Heard && !s.Legacy, "started over");
            Nothing(s.Size(100, 30, t));
            True(s.Legacy, "no hello this time: an older Ward");
        }

        static void ScreenWithoutOwnSize()
        {
            DateTime t = Start();
            Screen s = new Screen(0, 0);
            True(!s.Holding, "nothing to hold for");
            Nothing(s.Size(120, 40, t));
            Equal("x", Drawn(s.Output(B("x"), t)));
        }

        static void StatusTexts()
        {
            Equal("Ward: подключаемся к steep-pc", Status.Title(Link.Connecting, "steep-pc"));
            Equal("Ward: ждём Ward на steep-pc (сессия запускается, агент ещё не стартовал)", Status.Title(Link.Waiting, "steep-pc"));
            Equal("Ward: подключено", Status.Title(Link.Live, "steep-pc"));
            Equal("Ward! на steep-pc Ward старой версии: экран может задваиваться, обновите Ward там", Status.Title(Link.Legacy, "steep-pc"));
            Equal("Ward! Ward на steep-pc отключился, переподключаемся", Status.Title(Link.Reconnecting, "steep-pc"));
            Equal("Ward: трансляция закончилась", Status.Title(Link.Ended, "steep-pc"));
            Equal("подключаемся к машине сессии", Status.Text(Link.Connecting, ""));
            Equal("ждём Ward на машине сессии (сессия запускается, агент ещё не стартовал)", Status.Text(Link.Waiting, null));
        }

        /// A screen of a newer Ward that is live at 100x30.
        static Screen Live(DateTime t)
        {
            Screen s = new Screen(100, 30);
            s.Hello(true, true);
            s.Size(120, 40, t);
            s.Size(100, 30, t);
            True(!s.Holding, "live");
            return s;
        }

        static DateTime Start()
        {
            return new DateTime(2026, 11, 5, 5, 13, 0, DateTimeKind.Utc);
        }

        static void Check(string name, Action test)
        {
            try
            {
                test();
                lines.Add("ok\t" + name);
            }
            catch (Exception e)
            {
                failures++;
                lines.Add("FAIL\t" + name + "\t" + e.Message);
            }
        }

        static void Nothing(Step step)
        {
            if (step.Clear || step.Warn || step.Draw.Count > 0)
            {
                throw new Exception("expected nothing to do, got clear=" + step.Clear + " warn=" + step.Warn + " draw=" + Show(Drawn(step)));
            }
        }

        static void Equal(string expected, string actual)
        {
            if (expected != actual)
            {
                throw new Exception("expected " + Show(expected) + ", got " + Show(actual));
            }
        }

        static void True(bool value, string what)
        {
            if (!value)
            {
                throw new Exception("not so: " + what);
            }
        }

        static string Show(string text)
        {
            return text == null ? "null" : "\"" + text.Replace("\u001b", "\\e").Replace("\u0007", "\\a") + "\"";
        }

        static string Feed(params string[] pieces)
        {
            Filter filter = new Filter();
            StringBuilder kept = new StringBuilder();
            foreach (string piece in pieces)
            {
                kept.Append(filter.Feed(piece));
            }
            return kept.ToString();
        }

        static byte[] B(string text)
        {
            return Encoding.UTF8.GetBytes(text);
        }

        static string Drawn(Step step)
        {
            StringBuilder drawn = new StringBuilder();
            foreach (byte[] chunk in step.Draw)
            {
                drawn.Append(Encoding.UTF8.GetString(chunk));
            }
            return drawn.ToString();
        }
    }
}
