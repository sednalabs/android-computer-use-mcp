package com.sednalabs.mcp.acceptance;

import android.app.Activity;
import android.app.AlertDialog;
import android.graphics.Color;
import android.os.Bundle;
import android.view.Gravity;
import android.view.MotionEvent;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

/** Disposable, offline UI fixture. No permissions, account data or network use. */
public final class MainActivity extends Activity {
    private int count;
    private int maxPointers;
    private String text = "";
    private EditText input;
    private TextView counter;
    private TextView pointerCount;

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        showControls();
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }

    private LinearLayout page() {
        LinearLayout page = new LinearLayout(this);
        page.setOrientation(LinearLayout.VERTICAL);
        page.setPadding(dp(24), dp(28), dp(24), dp(20));
        page.setFitsSystemWindows(true);
        page.setBackgroundColor(Color.WHITE);
        page.setFocusableInTouchMode(true);
        return page;
    }

    private TextView label(String text) {
        TextView label = new TextView(this);
        label.setText(text);
        label.setTextSize(18);
        label.setTextColor(Color.BLACK);
        label.setPadding(0, dp(6), 0, dp(6));
        return label;
    }

    private Button button(int id, String text, View.OnClickListener listener) {
        Button button = new Button(this);
        button.setId(id);
        button.setText(text);
        button.setAllCaps(false);
        button.setOnClickListener(listener);
        return button;
    }

    private void showControls() {
        LinearLayout page = page();
        page.addView(label("Android MCP acceptance"));
        counter = label("Count: " + count);
        counter.setId(R.id.counter);
        page.addView(counter);
        page.addView(button(R.id.increment, "Increment", view -> {
            count++;
            counter.setText("Count: " + count);
        }));
        input = new EditText(this);
        input.setId(R.id.text_input);
        input.setContentDescription("Acceptance text input");
        input.setSingleLine(true);
        input.setHint("Enter test text");
        input.setText(text);
        page.addView(input);
        page.addView(button(R.id.dialog, "Open dialog", view ->
            new AlertDialog.Builder(this).setTitle("Acceptance dialog")
                .setMessage("Dialog is visible")
                .setPositiveButton("Close dialog", (dialog, which) -> dialog.dismiss())
                .show()));
        page.addView(button(R.id.scroll, "Open scroll page", view -> {
            text = input.getText().toString();
            showScrollPage();
        }));
        pointerCount = label("Max pointers: " + maxPointers);
        pointerCount.setId(R.id.pointer_count);
        page.addView(pointerCount);
        TextView pad = label("Touch pad");
        pad.setId(R.id.touch_pad);
        pad.setGravity(Gravity.CENTER);
        pad.setContentDescription("Acceptance touch pad");
        pad.setBackgroundColor(Color.rgb(220, 235, 245));
        pad.setOnTouchListener((view, event) -> {
            maxPointers = Math.max(maxPointers, event.getPointerCount());
            pointerCount.setText("Max pointers: " + maxPointers);
            return true;
        });
        page.addView(pad, new LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.MATCH_PARENT, dp(150)));
        setContentView(page);
        page.requestFocus();
    }

    private void showScrollPage() {
        ScrollView scroll = new ScrollView(this);
        scroll.setFillViewport(true);
        LinearLayout page = page();
        page.addView(label("Scroll acceptance page"));
        for (int i = 1; i <= 18; i++) {
            TextView row = label("Scroll row " + i);
            row.setMinHeight(dp(64));
            page.addView(row);
        }
        TextView end = label("Scroll destination reached");
        end.setId(R.id.scroll_end);
        page.addView(end);
        page.addView(button(R.id.back_to_controls, "Back to controls", view -> showControls()));
        scroll.addView(page);
        setContentView(scroll);
    }
}
