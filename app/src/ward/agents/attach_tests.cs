// The checks of the viewer's C# (attach.ps1), compiled after it into one source: it has the
// usings. attach_tests.rs runs them under Windows PowerShell, as the tab runs the viewer, and
// reads the file Suite.Run writes: a line per check.

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

        static void Equal(string expected, string actual)
        {
            if (expected != actual)
            {
                throw new Exception("expected " + Show(expected) + ", got " + Show(actual));
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
    }
}
