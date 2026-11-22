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
}
