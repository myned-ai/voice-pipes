$ErrorActionPreference = 'Stop'

$models = @(
    "models/gemini-2.0-flash",
    "models/gemini-2.0-flash-001",
    "models/gemini-2.5-flash",
    "models/gemini-2.5-flash-001",
    "models/gemini-2.5-flash-preview-tts",
    "models/gemini-3.1-flash-live-preview",
    "models/gemini-2.5-flash-native-audio-preview-12-2025",
    "models/gemini-2.5-flash-native-audio-latest"
)

$apiKey = $env:GEMINI_API_KEY
if (-not $apiKey) {
    Write-Host "GEMINI_API_KEY is missing."
    exit 1
}

Add-Type -AssemblyName System.Web

foreach ($model in $models) {
    Write-Host "Testing model: $model"
    
    $uri = [Uri]"wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key=$apiKey"
    
    $ws = New-Object System.Net.WebSockets.ClientWebSocket
    $cts = New-Object System.Threading.CancellationTokenSource(5000)
    
    try {
        $task = $ws.ConnectAsync($uri, $cts.Token)
        $task.Wait()
        
        $json = @{
            setup = @{
                model = $model
            }
        } | ConvertTo-Json -Depth 5 -Compress
        
        $bytes = [System.Text.Encoding]::UTF8.GetBytes($json)
        $segment = New-Object System.ArraySegment[byte]($bytes, 0, $bytes.Length)
        
        $sendTask = $ws.SendAsync($segment, [System.Net.WebSockets.WebSocketMessageType]::Text, $true, $cts.Token)
        $sendTask.Wait()
        
        $buffer = New-Object byte[] 4096
        $receiveSegment = New-Object System.ArraySegment[byte]($buffer, 0, $buffer.Length)
        
        $receiveTask = $ws.ReceiveAsync($receiveSegment, $cts.Token)
        $receiveTask.Wait()
        
        if ($ws.State -eq 'Closed' -or $receiveTask.Result.MessageType -eq 'Close') {
            Write-Host "  -> CLOSED. Code: $($ws.CloseStatus), Reason: $($ws.CloseStatusDescription)"
        } else {
            $response = [System.Text.Encoding]::UTF8.GetString($buffer, 0, $receiveTask.Result.Count)
            Write-Host "  -> SUCCESS/OPEN! Response: $response"
        }
    } catch {
        Write-Host "  -> EXCEPTION: $($_.Exception.Message)"
        if ($ws.CloseStatus) {
            Write-Host "  -> Close Status: $($ws.CloseStatus), Reason: $($ws.CloseStatusDescription)"
        }
    } finally {
        if ($ws.State -ne [System.Net.WebSockets.WebSocketState]::Closed) {
            # don't care about graceful close
            $ws.Abort()
        }
        $ws.Dispose()
        $cts.Dispose()
    }
}
