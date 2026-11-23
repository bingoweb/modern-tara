namespace Fixture;

public class Old
{
    private readonly string _name;
    public Old(string name) { _name = name; } // OLD:IDE0290:modern
    public int[] Array() { int[] a = new int[] { 1, 2 }; return a; } // OLD:IDE0300:modern
    public int[] Modern() => [1, 2]; // DECOY
    public static void Guard(string? s) { if (s == null) throw new ArgumentNullException(nameof(s)); } // OLD:CA1510:modern
    public static void Guarded(string? s) { ArgumentNullException.ThrowIfNull(s); } // DECOY
    public int Roll() => new Random().Next(); // OLD:csharp-new-random:modern
    public int Shared() => Random.Shared.Next(); // DECOY
    public string Name => _name;
    private static string? Find() => null;
    public static string Fill(string? s) { if (s == null) s = "x"; return s; } // OLD:IDE0074:modern
    public static string Filled(string? s) { s ??= "x"; return s; } // DECOY
    public static string? Trimmed(string? s) => s != null ? s.Trim() : null; // OLD:IDE0031:modern
    public static string? Trims(string? s) => s?.Trim(); // DECOY
    public static string Pick(string? s) => s != null ? s : "y"; // OLD:IDE0029:modern
    public static int Orr(int? n) => n.HasValue ? n.Value : 3; // OLD:IDE0030:modern
    public static string Need() { var w = Find(); if (w == null) { throw new InvalidOperationException(); } return w; } // OLD:IDE0270:modern
    public static int Count(List<int> l) => l.Count(); // OLD:CA1829:modern
    public static int Size(List<int> l) => l.Count; // DECOY
    public static int Idx(string t) => t.IndexOf("a", StringComparison.Ordinal); // OLD:CA1865:modern
    public static bool Starts(string t) => t.StartsWith("a"); // OLD:CA1866:modern
    public static bool StartsChar(string t) => t.StartsWith('a'); // DECOY
    public static bool Has(string t) => t.Contains("a"); // OLD:CA1847:modern
    public static bool IsHead(string t) => t.IndexOf("xy") == 0; // OLD:CA1858:modern
    public static string Hex(byte[] b) => BitConverter.ToString(b).Replace("-", ""); // OLD:CA1872:modern
    public static string Hexed(byte[] b) => Convert.ToHexString(b); // DECOY
}
